//! Explicit LUT export, only after the user's choice in the save dialog.
use crate::{dialog, jobs::Job, App};
use eframe::egui;
#[derive(Default)]
pub struct Lut {
    pub open: bool,
    pub busy: bool,
    pub name: String,
    pub message: String,
    /// opened from "Film & Look": the look chosen for the photo (a film still goes the darkroom way)
    pub look: bool,
    /// test hook GRAINEO_LOOK_LUT already ran
    pub hook_done: bool,
}
impl App {
    pub(crate) fn darkroom_lut_open(&mut self) {
        let recipe = self.darkroom.recipes.selection.and_then(|q| self.darkroom_recipe_of(q));
        self.darkroom.lut.name = recipe.map(|r| r.name).unwrap_or_else(|| {
            self.current
                .as_ref()
                .and_then(|c| develop::film::stock(&self.entries[c.index].settings.film.stock))
                .map_or(crate::tr("Film-Look").into(), |f| f.name.clone())
        });
        self.darkroom.lut.look = false;
        self.darkroom.lut.message.clear();
        self.darkroom.lut.open = true;
    }
    /// What "Look als LUT speichern" can export for the current photo: Ok(()) or the reason why not.
    pub(crate) fn look_lut_available(&self) -> Result<(), &'static str> {
        let Some(c) = &self.current else { return Err("Kein Foto geöffnet") };
        let s = &self.entries[c.index].settings;
        if s.film.is_active() {
            return Ok(());
        }
        match &c.loaded.previews.foreign {
            Some(doc) if doc.look_exportable(s) => Ok(()),
            Some(_) => Err("Kein Look gewählt"),
            None => Err("Nur für Filme und Looks von RAW-Fotos"),
        }
    }
    /// Entry from "Film & Look".
    pub(crate) fn film_look_lut_open(&mut self) {
        if self.look_lut_available().is_err() {
            return;
        }
        let Some(c) = &self.current else { return };
        self.darkroom.lut.name = crate::film_look::current_look_name(
            &self.entries[c.index].settings,
            c.loaded.previews.foreign.as_deref(),
            &c.loaded.camera_mode,
        );
        self.darkroom.lut.look = true;
        self.darkroom.lut.message.clear();
        self.darkroom.lut.open = true;
    }
    pub(crate) fn darkroom_lut_dialog(&mut self, ctx: &egui::Context) {
        if !self.darkroom.lut.open {
            return;
        }
        if let Some(path) = crate::file_dialog::fetch_one("dk-lut-speichern") {
            let name = self.darkroom.lut.name.clone();
            let current = self.current.as_ref().and_then(|c| self.entries.get(c.index).map(|e| (e.settings.clone(), c.loaded.previews.foreign.clone())));
            let job = match current {
                // look of a RAW photo (a film is exported like in the darkroom)
                Some((settings, Some(doc))) if self.darkroom.lut.look && !settings.film.is_active() => Some(Job::ExportLookLut { doc, settings, name, path }),
                Some((settings, _)) => Some(Job::ExportLut { settings: settings.film, name, path }),
                None => None,
            };
            if let Some(job) = job {
                self.darkroom.lut.busy = true;
                self.darkroom.lut.message = crate::tr("Wird berechnet …").into();
                self.worker.send(job);
            }
        }
        let l = &mut self.darkroom.lut;
        let busy = l.busy;
        let enabled = !busy && !l.name.trim().is_empty();
        let look_only = l.look && self.current.as_ref().and_then(|c| self.entries.get(c.index)).is_some_and(|e| !e.settings.film.is_active());
        let file_name = file_name_of(&l.name);
        let answer = dialog::show(
            ctx,
            "film-lut-export",
            crate::tr("Look als LUT speichern"),
            480.,
            |ui| {
                dialog::input(ui, crate::tr("Name"), &mut l.name);
                dialog::text(ui, crate::tr("33³ Stützpunkte · sRGB → sRGB"));
                if look_only {
                    dialog::hint(ui, crate::tr("Enthalten: der gewählte Look mit seiner Intensität. Nicht enthalten: Belichtung, Weißabgleich, Tonkurve, Sättigung, lokale Korrekturen, Schärfung, Rauschminderung, Körnung und Hof."));
                } else {
                    dialog::hint(ui,crate::tr("Körnung und Hof sind nicht enthalten. Auch Weichzeichner, Objektivunschärfe und lokale Masken lassen sich nicht in einer Farb-LUT speichern."));
                }
                if !l.message.is_empty() {
                    dialog::text(ui, &l.message);
                }
            },
            dialog::Buttons::confirm(
                if busy {
                    crate::tr("WIRD BERECHNET …")
                } else {
                    crate::tr("SPEICHERN …")
                },
                enabled,
            ),
        );
        match answer {
            dialog::Answer::Cancel => self.darkroom.lut.open = false,
            dialog::Answer::Primary => {
                crate::file_dialog::save(ctx, "dk-lut-speichern", |d| d.set_title(crate::tr("Look als LUT speichern")).set_file_name(file_name).add_filter(crate::tr("Farb-LUT"), &["cube"]));
            }
            _ => {}
        }
    }
}

/// Suggested file name for the save dialog from the look name.
fn file_name_of(name: &str) -> String {
    let base: String = name.trim().chars().map(|c| if c.is_control() || "/\\:*?\"<>|".contains(c) { '-' } else { c }).collect();
    let base = base.trim_matches(|c: char| c == '.' || c == '-' || c.is_whitespace());
    format!("{}.cube", if base.is_empty() { "film-look" } else { base })
}
