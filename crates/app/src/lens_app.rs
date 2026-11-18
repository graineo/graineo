//! The lens windows seen from the main window: carries out requests of the assignment dialog, the assistant and the
//! window OBJEKTIVE (all of them draw from thread-local state, see `lens_assign`).

use super::*;
use crate::lens_assign::{self, Applied, Choice, Identity, Request, Scope};

impl App {
    /// Camera and lens of the open photo; `None` for finished images (JPEG, HEIC …: no own profile) and when nothing is open.
    pub(crate) fn lens_identity(&self) -> Option<(usize, Identity)> {
        let c = self.current.as_ref()?;
        match &c.loaded.previews.foreign {
            Some(doc) if doc.finished => None,
            Some(doc) => {
                let (maker, model, lens) = doc.lens_identity();
                Some((c.index, Identity::new(maker, model, lens)))
            }
            None if !c.loaded.cal.is_prepared() => Some((c.index, Identity::foveon(&c.loaded.cal.camera))),
            None => None,
        }
    }

    /// Call every frame: requests of the lens windows, then the windows themselves.
    pub(crate) fn lens_ui(&mut self, ctx: &egui::Context) {
        for r in lens_assign::take_requests() {
            match r {
                Request::Assign => match self.lens_identity() {
                    Some((i, id)) => lens_assign::open(ctx, id, self.entries[i].settings.raw.lens_profile.as_deref(), self.selection().len()),
                    None => self.status = crate::tr("Für dieses Foto gibt es kein Objektivprofil").to_string(),
                },
                Request::Room => crate::lens_room::open(ctx),
                Request::Wizard => crate::lens_wizard::open(),
                Request::Apply { profile, ident, rule, photo } => {
                    if rule {
                        self.lens_set_rule(&ident, &Choice::Profile(profile.clone()));
                    }
                    if photo {
                        if let Some(i) = self.current.as_ref().map(|c| c.index) {
                            self.lens_set_pin(vec![i], Some(profile));
                        }
                    }
                }
            }
        }
        if let Some(a) = lens_assign::ui(ctx) {
            self.lens_carry_out(a);
        }
        crate::lens_wizard::ui(ctx);
        crate::lens_room::ui(ctx);
    }

    fn lens_carry_out(&mut self, a: Applied) {
        let cur = self.current.as_ref().map(|c| c.index);
        match a.scope {
            Scope::Photo => self.lens_set_pin(cur.into_iter().collect(), a.choice.pin()),
            Scope::Selection => {
                let v = self.selection();
                self.lens_set_pin_command(v, a.choice.pin());
            }
            Scope::Rule => {
                self.lens_set_rule(&a.ident, &a.choice);
                // the photo in front of the user shows the result: its own assignment would hide the default
                if let Some(i) = cur.filter(|i| self.entries[*i].settings.raw.lens_profile.is_some()) {
                    self.lens_set_pin(vec![i], None);
                }
            }
        }
    }

    fn lens_set_rule(&mut self, ident: &Identity, choice: &Choice) {
        match lens_assign::set_rule(ident, choice) {
            Ok(()) => {
                // photos that are open render again (the default is not part of their settings)
                self.rendered_for = None;
                self.status = crate::tr("Vorgabe für das Objektiv gespeichert").to_string();
            }
            Err(e) => self.status = crate::trf!("Vorgabe nicht gespeichert: {err}", err = e),
        }
    }

    /// The assignment for several photos: one command like pasting settings (a step in each photo's history, one ⌘Z
    /// for all of them).
    fn lens_set_pin_command(&mut self, indices: Vec<usize>, pin: Option<String>) {
        let (targets, unreachable) = self.without_unreachable(indices);
        let Some(&first) = targets.first() else {
            if unreachable > 0 {
                self.status = crate::trf!("Zugewiesen; {a} übersprungen: Original nicht erreichbar", a = unreachable);
            }
            return;
        };
        self.ensure_entry(first);
        let before = self.entries[first].settings.clone();
        let mut after = before.clone();
        after.raw.lens_profile = pin.clone();
        let (kind, label) = crate::history::describe(&before, &after);
        let cmd = crate::history::Command::new(crate::history::Hint { kind, label });
        self.settings_command(&targets, cmd, move |s| {
            let mut s = s.clone();
            s.raw.lens_profile = pin.clone();
            s
        });
        if unreachable > 0 {
            self.status = crate::trf!("Zugewiesen; {a} übersprungen: Original nicht erreichbar", a = unreachable);
        }
        self.compute_thumbnails();
    }

    /// Set the assignment (`None` = automatic, `Some("")` = none) on the given photos, like pasting settings.
    fn lens_set_pin(&mut self, indices: Vec<usize>, pin: Option<String>) {
        let cur = self.current.as_ref().map(|c| c.index);
        let (targets, unreachable) = self.without_unreachable(indices);
        for i in targets {
            self.ensure_entry(i);
            self.entries[i].settings.raw.lens_profile = pin.clone();
            if Some(i) != cur {
                if let Err(err) = save_entry(&self.entries[i]) {
                    self.status = crate::trf!("Einstellungen nicht gespeichert: {err}", err = err);
                }
                self.queue_thumbnail(i);
            }
        }
        if unreachable > 0 {
            self.status = crate::trf!("Zugewiesen; {a} übersprungen: Original nicht erreichbar", a = unreachable);
        }
        self.compute_thumbnails();
    }
}
