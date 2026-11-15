//! Side columns of the import window: on the left the source (cards, cameras, folders, recent folders), counters and
//! filters; on the right the operation, target, subfolders, second copy and "apply on import" (collection, develop
//! preset, keywords, rating, label, copyright). Both have a fixed order: for "Hinzufügen" the target rows stay and are
//! dimmed. "Hinzufügen" shows the folders in the middle column instead of the grid ([`folders`]).

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
        ui.add_sized([58.0, 16.0], egui::Label::new(egui::RichText::new(n).color(lr::TEXT)).halign(egui::Align::Max));
        ui.label(egui::RichText::new(crate::tr(text)).color(lr::TEXT_DIM));
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
                small(ui, &crate::trf!("„{n}“ wird gelesen …", n = name), lr::TEXT_DIM);
            });
        }
        camera::sources(ui, imp, pick);
        let r = ui.button(crate::tr("Ordner …"));
        lr::hover_tip(ui, &r, "Einen Ordner als Quelle wählen");
        if r.clicked() {
            pick.source = true;
        }
        if let Some(q) = imp.source.as_ref().filter(|q| !camera::is_camera(Some(q))) {
            small(ui, &q.display().to_string(), lr::TEXT_DIM);
        }
        let recent: Vec<PathBuf> = recent_sources().into_iter().filter(|p| Some(p) != imp.source.as_ref()).take(4).collect();
        ui.add_space(4.0);
        heading(ui, "Zuletzt");
        if recent.is_empty() {
            small(ui, crate::tr("–"), lr::TEXT_DIM);
        }
        for p in recent {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string());
            let r = lr::list_item(ui, &name, false);
            lr::hover_tip(ui, &r, &p.display().to_string());
            if r.clicked() {
                pick.card = Some(p);
            }
        }
        ui.add_space(6.0);
        ui.separator();
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
                small(ui, &t, lr::TEXT_DIM);
            });
        }
        let r = ui.scope(|ui| counter(ui, Some(o.photos), "Fotos gefunden")).response;
        lr::hover_tip(ui, &r, "Ein Foto ist eine Aufnahme: RAW und JPG derselben Aufnahme zählen als ein Foto");
        let known = |n: usize| (!checking).then_some(n);
        ui.horizontal(|ui| {
            let n = known(o.new).map(dialog::number).unwrap_or_else(|| "…".into());
            ui.add_sized([58.0, 16.0], egui::Label::new(egui::RichText::new(n).color(lr::TEXT)).halign(egui::Align::Max));
            // no size while nothing is new (the size format shows at least 1 MB)
            let t = if o.new_bytes > 0 && !checking { crate::trf!("neu · {a}", a = dialog::size(o.new_bytes)) } else { crate::tr("neu").to_string() };
            ui.label(egui::RichText::new(t).color(lr::TEXT_DIM));
        });
        let r = ui.scope(|ui| counter(ui, known(o.there), "schon da")).response;
        lr::hover_tip(ui, &r, "Schon in der Bibliothek (gleicher Inhalt, egal unter welchem Namen und in welchem Ordner); bei RAW+JPG entscheidet das RAW – nicht angehakt");
        let r = ui.scope(|ui| counter(ui, known(o.probable), "vermutlich da")).response;
        lr::hover_tip(ui, &r, "Gleiche Größe und gleicher Name oder Aufnahmezeit wie ein Foto der Bibliothek, dessen Laufwerk nicht verbunden ist – nicht angehakt");
        let r = ui.scope(|ui| counter(ui, known(o.identical), "identisch")).response;
        lr::hover_tip(ui, &r, "Bytegleiche Kopien in dieser Quelle: nur die erste wird importiert");
        ui.add_space(6.0);
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
            heading(ui, "Ziel");
            crate::help::info(ui, "import", "Fotos importieren", &crate::help::IMPORT);
        });
        // from a camera only copying: nothing is ever deleted on it, and it has no folders to add
        let from_camera = camera::is_camera(imp.source.as_deref());
        if from_camera {
            imp.mode = Mode::CopyFiles;
        }
        let r = ui
            .add_enabled_ui(!from_camera, |ui| {
                if lr::segmented(ui, &mut imp.mode, &[(Mode::InPlace, crate::tr("Hinzufügen")), (Mode::CopyFiles, crate::tr("Kopieren")), (Mode::MoveFiles, crate::tr("Verschieben"))]) {
                    imp.mode_automatic = false;
                }
            })
            .response;
        lr::hover_tip(
            ui,
            &r,
            if from_camera { "Von einer Kamera wird immer kopiert: geprüft (SHA-256) ins Ziel, auf der Kamera bleibt alles unverändert" } else { match imp.mode {
                Mode::InPlace => "Hinzufügen: Fotos bleiben am Ort, ihre Ordner kommen in die Bibliothek",
                Mode::CopyFiles => "Kopieren: geprüft (SHA-256) ins Ziel, die Quelle bleibt unverändert",
                Mode::MoveFiles => "Verschieben: wie Kopieren, danach die Originale in den Papierkorb – erst wenn alle Kopien eines Fotos geprüft sind",
            } },
        );
        let copies = imp.mode != Mode::InPlace;
        ui.add_space(4.0);
        ui.add_enabled_ui(copies, |ui| {
            ui.horizontal(|ui| {
                heading(ui, "Ablegen in");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(crate::tr("Ändern …")).clicked() {
                        pick.target = true;
                    }
                });
            });
            if !copies {
                small(ui, crate::tr("– Fotos bleiben am Ort –"), lr::TEXT_DIM);
                small(ui, "", lr::TEXT_DIM);
            } else {
                let t = imp.target.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| crate::tr("noch kein Ordner gewählt").into());
                small(ui, &t, lr::TEXT);
                // a fixed line below the path: problem, or free space and what is needed
                let need = imp.chosen_tally().bytes;
                let line = if let (None, Some(p)) = (&imp.target, &imp.target_missing) {
                    (crate::trf!("„{p}“ ist nicht erreichbar", p = p.display()), Color32::from_rgb(0xe0, 0xb0, 0x40))
                } else if target_gone {
                    (crate::tr("Zielordner gerade nicht erreichbar").to_string(), Color32::from_rgb(0xe0, 0xb0, 0x40))
                } else if let Some(z) = imp.target.clone() {
                    match free_space(imp, &z) {
                        // nothing chosen yet: no "needs 1 MB" (the size format shows at least 1 MB)
                        Some(f) if need == 0 => (crate::trf!("{a} frei", a = dialog::size(f)), lr::TEXT_DIM),
                        Some(f) if f < need => (crate::trf!("{a} frei · braucht {b} – zu wenig Platz", a = dialog::size(f), b = dialog::size(need)), Color32::from_rgb(0xe0, 0x5c, 0x50)),
                        Some(f) => (crate::trf!("{a} frei · braucht {b}", a = dialog::size(f), b = dialog::size(need)), lr::TEXT_DIM),
                        None => (crate::trf!("braucht {b}", b = dialog::size(need)), lr::TEXT_DIM),
                    }
                } else {
                    (String::new(), lr::TEXT_DIM)
                };
                small(ui, &line.0, line.1);
            }
            ui.add_space(2.0);
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
            small(ui, &example.unwrap_or_default(), lr::TEXT_DIM);
            ui.add_space(2.0);
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
            let t = imp.backup.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| crate::tr("keine").into());
            small(ui, &t, if imp.backup.is_some() { lr::TEXT } else { lr::TEXT_DIM });
        });
        ui.add_space(6.0);
        ui.separator();
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
            ui.add_sized([label_w, 20.0], egui::Label::new(egui::RichText::new(crate::tr(label)).color(lr::TEXT_DIM)).halign(egui::Align::Min));
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
        let names = ["Kein Etikett", "Etikett Rot", "Etikett Gelb", "Etikett Grün", "Etikett Blau", "Etikett Lila"];
        let items: Vec<(Color32, &str)> = library::label::COLORS.iter().zip(names).map(|(c, n)| (Color32::from_rgb(c[0], c[1], c[2]), n)).collect();
        ui.allocate_ui(egui::vec2(field_w, 24.0), |ui| {
            if let Some(i) = lr::color_chips(ui, "imp-etikett", &items, Some(imp.label as usize)) {
                imp.label = i as u8;
            }
        });
    });
    row(ui, "Copyright", &mut |ui| {
        crate::lr::edit_field(ui, egui::TextEdit::singleline(&mut imp.copyright).hint_text(crate::tr("z. B. © 2026 Name")).desired_width(field_w));
    });
}

/// "Hinzufügen": the folders that are added (checked per folder; a folder whose photos are all in the library already
/// starts unchecked), and where they go (inside a root folder, or the source becomes a new one).
pub(super) fn folders(ui: &mut egui::Ui, imp: &mut Import) {
    heading(ui, "Hinzugefügt wird");
    let source = imp.source.clone().unwrap_or_default();
    egui::Frame::new().fill(lr::PANEL_DARK).inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
        ui.set_min_size(egui::vec2(ui.available_width(), ui.available_height() - 60.0));
        egui::ScrollArea::vertical().id_salt("import-ordner").auto_shrink([false, false]).max_height(ui.available_height() - 60.0).show(ui, |ui| {
            if imp.folders.is_empty() {
                small(ui, if imp.scanning { crate::tr("liest die Fotos …") } else { crate::tr("Keine Ordner mit Fotos.") }, lr::TEXT_DIM);
            }
            for (p, n, all_there) in imp.folders.clone() {
                let rel = p.strip_prefix(&source).ok().map(|r| r.display().to_string()).filter(|r| !r.is_empty()).unwrap_or_else(|| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
                let mut on = imp.folders_on.get(&p).copied().unwrap_or(false);
                ui.horizontal(|ui| {
                    if ui.checkbox(&mut on, rel).changed() {
                        imp.folders_on.insert(p.clone(), on);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new(dialog::count(n, crate::tr("Foto"), crate::tr("Fotos"))).color(lr::TEXT_DIM));
                        if all_there {
                            ui.label(egui::RichText::new(crate::tr("schon in der Bibliothek")).color(Color32::from_rgb(0x8f, 0xc0, 0x8a)));
                        }
                    });
                });
            }
        });
    });
    ui.add_space(6.0);
    if imp.source_in_library {
        small(ui, crate::tr("liegt in einem Ordner der Bibliothek"), lr::TEXT_DIM);
    } else if imp.source.is_some() {
        small(ui, crate::tr("als neuer Stammordner:"), lr::TEXT_DIM);
        small(ui, &source.display().to_string(), lr::TEXT);
    }
}
