//! Multi-selection in the grid: **first adjust, then apply.** The sliders only change the active photo (live, fast).
//! The panel at the top of the Edit tool ("Selection: n photos") lists what changed on the active photo since it
//! became active (or since the last apply) and offers "Apply to n photos" with the choice
//! - **relative** (numbers get the same change: +0.3 EV is added to each photo's own value, differences stay), or
//! - **equal** (every photo gets the active photo's value).
//! Choices and switches (colour mode, black & white …) are always copied. Large selections ask first
//! ([`CONFIRM_FROM`]). Applying runs in the background, block by block, with progress and a cancel button
//! (`multi_apply.rs`); one ⌘Z takes it back for all photos. Previews follow afterwards, visible cells first.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use develop::Settings;
use eframe::egui;
use serde_json::Value;

use crate::multi_apply::{Msg, Runner, Step, Target};
use crate::settings_group::Change;
use crate::{lr, App};

/// From this number of target photos "Apply" asks first.
pub const CONFIRM_FROM: usize = 200;

/// Up to this many photos a settings command (paste, undo …) is applied right away in the UI thread; more go through
/// the background ([`crate::multi_apply`]).
pub const RIGHT_AWAY_MAX: usize = 50;

/// Previews re-rendered per background job (the next part follows when it is done; visible cells first).
const THUMB_CHUNK: usize = 4;

/// Height of the band "Selection: n photos" (the same while applying: nothing moves).
pub const BAND_H: f32 = 84.0;
const BAND_FILL: egui::Color32 = egui::Color32::from_gray(0x21);
const BAND_STROKE: egui::Color32 = egui::Color32::from_gray(0x3a);
/// Accent of the band: the theme's light accent (gold is kept for the main button of the window)
const BAND_ACCENT: egui::Color32 = lr::ACCENT;
const BAND_BUTTON: egui::Color32 = egui::Color32::from_gray(0x4a);

/// Settings key of the chosen way of applying.
const MODE_KEY: &str = "multiApplyMode";

/// How numbers go onto the other photos.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    /// the same change (differences between the photos stay)
    #[default]
    Relative,
    /// the active photo's value
    Equal,
}

/// A running background request as the UI sees it.
struct Job {
    id: u64,
    total: usize,
    done: usize,
    record: bool,
    changes: Vec<Change>,
}

/// State of "Apply to the selection" (one per app).
#[derive(Default)]
pub struct Multi {
    runner: Runner,
    jobs: Vec<Job>,
    mode: Option<Mode>,
    /// the question before applying to many photos is open
    confirm: bool,
    /// ⌘Z while applying: cancel, then take back what was saved
    undo_after: bool,
    ctx: Option<egui::Context>,
}

impl Multi {
    pub fn mode(&mut self) -> Mode {
        *self.mode.get_or_insert_with(|| if library::settings::value(MODE_KEY).as_str() == Some("equal") { Mode::Equal } else { Mode::Relative })
    }

    fn set_mode(&mut self, m: Mode) {
        self.mode = Some(m);
        library::settings::set(MODE_KEY, serde_json::json!(if m == Mode::Equal { "equal" } else { "relative" }));
    }

    /// Progress of the background work: (saved, of).
    pub fn progress(&self) -> Option<(usize, usize)> {
        let j = self.jobs.first()?;
        Some((j.done, j.total))
    }
}

/// Transfer the change `old → new` to `target` (relative: numbers get the same change; otherwise they get the new
/// value). Choices, switches and texts are always copied; what did not change stays as it is.
pub fn transfer_with(old: &Value, new: &Value, target: &mut Value, relative: bool) {
    if old == new {
        return;
    }
    match (old, new, &mut *target) {
        (Value::Number(a), Value::Number(n), Value::Number(z)) => {
            if relative {
                if let (Some(a), Some(n), Some(zv)) = (a.as_f64(), n.as_f64(), z.as_f64()) {
                    if let Some(v) = serde_json::Number::from_f64(zv + (n - a)) {
                        *target = Value::Number(v);
                        return;
                    }
                }
            }
            *target = new.clone();
        }
        (Value::Array(a), Value::Array(n), Value::Array(z)) if a.len() == n.len() && n.len() == z.len() && a.iter().all(|x| x.is_number() || x.is_array()) => {
            for i in 0..z.len() {
                transfer_with(&a[i], &n[i], &mut z[i], relative);
            }
        }
        (Value::Object(a), Value::Object(n), Value::Object(z)) => {
            // a key that is gone went back to its default (fields left out when default, e.g. `binning`, `methods`)
            for k in a.keys().filter(|k| !n.contains_key(*k)) {
                z.remove(k);
            }
            for (k, nv) in n {
                let av = a.get(k).cloned().unwrap_or(Value::Null);
                match z.get_mut(k) {
                    Some(zv) => transfer_with(&av, nv, zv, relative),
                    None => {
                        z.insert(k.clone(), nv.clone());
                    }
                }
            }
        }
        _ => *target = new.clone(),
    }
}

/// [`transfer_with`], relative.
pub fn transfer(old: &Value, new: &Value, target: &mut Value) {
    transfer_with(old, new, target, true);
}

/// The step "change `base → now`" for every other photo.
pub fn step(base: &Settings, now: &Settings, mode: Mode) -> Step {
    let (a, n) = (serde_json::to_value(base).unwrap_or_default(), serde_json::to_value(now).unwrap_or_default());
    Arc::new(move |s: &Settings| {
        let mut z = serde_json::to_value(s).unwrap_or_default();
        transfer_with(&a, &n, &mut z, mode == Mode::Relative);
        serde_json::from_value::<Settings>(z).unwrap_or_else(|_| s.clone())
    })
}

fn signed(v: f64, places: usize) -> String {
    let t = crate::menu::i18n::format::decimal(crate::menu::i18n::language(), v.abs(), places);
    if v < 0.0 { format!("−{t}") } else { format!("+{t}") }
}

/// What changed from `base` to `now`, in short words ("Belichtung +0,30", "Farbe", "Tonwert" …).
pub fn describe(base: &Settings, now: &Settings) -> Vec<String> {
    let mut v = Vec::new();
    let mut seen = base.clone();
    if (now.exposure - base.exposure).abs() > 1e-9 {
        v.push(crate::trf!("Belichtung {a}", a = signed(now.exposure - base.exposure, 2)));
        seen.exposure = now.exposure;
    }
    if now.monochrome != base.monochrome {
        v.push(if now.monochrome { crate::tr("Schwarzweiß") } else { crate::tr("Farbe") }.to_string());
        seen.monochrome = now.monochrome;
    }
    if now.color_mode != base.color_mode {
        let name = now.color_mode.clone().unwrap_or_else(|| crate::tr("Kamera").to_string());
        v.push(crate::trf!("Farbmodus {a}", a = crate::tr(&name)));
        seen.color_mode = now.color_mode.clone();
    }
    for (name, _) in crate::paste::GROUPS {
        let only = crate::paste::Groups::from_names([name]);
        if crate::paste::merge(&seen, now, &only) != seen {
            v.push(crate::tr(name).to_string());
        }
    }
    if v.is_empty() && base != now {
        v.push(crate::tr("weitere Einstellungen").to_string());
    }
    v
}

fn stored(s: &Settings) -> Settings {
    Settings { view: develop::View::Developed, ..s.clone() }
}

impl App {
    /// Every frame after drawing the right panel: messages of the background work, the state at selection of the
    /// active photo (base of "Apply"), the question before applying to many photos.
    pub(crate) fn multi_follow(&mut self, ctx: &egui::Context) {
        self.multi.ctx = Some(ctx.clone());
        self.multi_messages();
        self.multi_test_hook(ctx);
        // while the active photo is (re)loaded there is no `current`: the base stays (a colour mode switch reloads it)
        let Some(cur) = self.current.as_ref().map(|c| c.index) else { return };
        let Some(image) = self.entries.get(cur).map(|e| e.image) else { return };
        if self.multi_base.as_ref().is_none_or(|(b, _)| *b != image) {
            self.multi_base = Some((image, stored(&self.entries[cur].settings)));
        }
        self.multi_confirm_dialog(ctx);
    }

    /// Test hook for screenshots (own data folder only): `GRAINEO_MULTI_TEST="mono=1;ev=0.3;apply=1"` (or `ask=1`: the button, with the question for many photos) –
    /// 3 s after the active photo is shown: select all photos of the grid, change colour mode / exposure of the active
    /// photo, optionally apply right away (progress) after one more second.
    fn multi_test_hook(&mut self, ctx: &egui::Context) {
        use std::sync::atomic::{AtomicU8, Ordering};
        static STAGE: AtomicU8 = AtomicU8::new(0);
        let Some(spec) = crate::env::var_os("GRAINEO_DATA_DIR").and_then(|_| crate::env::var("GRAINEO_MULTI_TEST").ok()) else { return };
        let get = |k: &str| spec.split(';').find_map(|p| p.split_once('=').filter(|(a, _)| *a == k).map(|(_, v)| v.to_string()));
        let now = ctx.input(|i| i.time);
        ctx.request_repaint_after(std::time::Duration::from_millis(200));
        static AT: std::sync::Mutex<f64> = std::sync::Mutex::new(0.0);
        let since = |at: &std::sync::Mutex<f64>| now - *at.lock().unwrap();
        match STAGE.load(Ordering::Relaxed) {
            0 if now > 3.0 && self.in_grid() => {
                self.choose_tool(crate::workspace::ToolTab::Edit);
                self.lib.select_all();
                STAGE.store(1, Ordering::Relaxed);
            }
            1 if self.loading.is_none() && self.current.is_some() && now - self.shown_at > 2.0 => {
                let cur = self.current.as_ref().map(|c| c.index).unwrap_or(0);
                if get("mono").as_deref() == Some("1") {
                    self.entries[cur].settings.monochrome = !self.entries[cur].settings.monochrome;
                }
                if let Some(ev) = get("ev").and_then(|v| v.parse::<f64>().ok()) {
                    self.entries[cur].settings.exposure += ev;
                }
                *AT.lock().unwrap() = now;
                STAGE.store(2, Ordering::Relaxed);
            }
            2 if since(&AT) > 2.0 => {
                if get("apply").as_deref() == Some("1") {
                    self.multi_apply_now();
                } else if get("ask").as_deref() == Some("1") {
                    self.multi_apply_request();
                }
                STAGE.store(3, Ordering::Relaxed);
            }
            _ => {}
        }
    }

    /// The change on the active photo since it became active (or since the last apply): (index, before, now).
    pub(crate) fn multi_change(&self) -> Option<(usize, Settings, Settings)> {
        // by image ID: the list may be rebuilt (other order) while the photo stays active
        let (image, base) = self.multi_base.as_ref()?;
        let cur = self.current.as_ref().map(|c| c.index).filter(|&i| self.entries.get(i).is_some_and(|e| e.image == *image))?;
        let now = stored(&self.entries[cur].settings);
        (now != *base).then(|| (cur, base.clone(), now))
    }

    /// Button "Apply to n photos": large selections ask first.
    pub(crate) fn multi_apply_request(&mut self) {
        let Some((cur, ..)) = self.multi_change() else { return };
        if self.multi_targets(cur).len() > CONFIRM_FROM {
            self.multi.confirm = true;
        } else {
            self.multi_apply_now();
        }
    }

    /// Hand the change of the active photo to the background for all other selected photos.
    pub(crate) fn multi_apply_now(&mut self) {
        let Some((cur, base, now)) = self.multi_change() else { return };
        let targets: Vec<Target> = self
            .multi_targets(cur)
            .into_iter()
            .map(|i| Target { image: self.entries[i].image, copy: self.entries[i].copy.clone(), path: self.entries[i].path.clone(), set: None })
            .collect();
        if targets.is_empty() {
            return;
        }
        let mode = self.multi.mode();
        self.multi_submit(vec![step(&base, &now, mode)], targets, true);
        // the change is handed over: the next one counts from here
        self.multi_base = Some((self.entries[cur].image, now));
        self.lib.rating_not_recent();
    }

    /// Hand a request to the background; its progress shows at the top of the Edit tool and in the title bar.
    pub(crate) fn multi_submit(&mut self, steps: Vec<Step>, targets: Vec<Target>, record: bool) {
        let total = targets.len();
        let ctx = self.multi.ctx.clone();
        let id = self.multi.runner.submit(ctx.as_ref(), steps, targets, record);
        if !self.multi.jobs.iter().any(|j| j.id == id) {
            self.multi.jobs.push(Job { id, total, done: 0, record, changes: Vec::new() });
        }
    }

    /// Is a change for several photos still being applied in the background?
    pub(crate) fn multi_busy(&self) -> bool {
        !self.multi.jobs.is_empty()
    }

    /// Cancel the background work (what is saved stays, undoable with ⌘Z).
    pub(crate) fn multi_cancel(&mut self) {
        self.multi.runner.cancel();
    }

    /// ⌘Z while applying: cancel first, then take back what was saved (in the frame the work has ended).
    pub(crate) fn multi_cancel_then_undo(&mut self) {
        self.multi.undo_after = true;
        self.multi_cancel();
        self.status = crate::tr("Wird abgebrochen …").to_string();
    }

    fn multi_messages(&mut self) {
        for m in self.multi.runner.take() {
            match m {
                Msg::Block { id, changes, done, total } => {
                    self.multi_block(&changes);
                    if let Some(j) = self.multi.jobs.iter_mut().find(|j| j.id == id) {
                        j.done = done;
                        j.total = total;
                        if j.record {
                            j.changes.extend(changes.into_iter().map(|(c, _)| c));
                        }
                    }
                }
                Msg::End { id, cancelled, errors, error } => {
                    let Some(p) = self.multi.jobs.iter().position(|j| j.id == id) else { continue };
                    let j = self.multi.jobs.remove(p);
                    let n = j.changes.len();
                    if j.record && n > 0 {
                        self.lib.rating_not_recent();
                        self.turn_undo.record_settings(j.changes);
                    }
                    if errors > 0 {
                        self.status = crate::trf!("{n} Fotos nicht gespeichert: {e}", n = errors, e = error.unwrap_or_default());
                    } else if cancelled && j.record {
                        self.status = crate::trf!("Abgebrochen: {k} von {n} Fotos geändert (⌘Z nimmt es zurück)", k = crate::library_ui::thousands(j.done), n = crate::library_ui::thousands(j.total));
                    } else if j.record {
                        self.status = crate::trf!("Auf {n} Fotos angewendet (⌘Z nimmt es zurück)", n = crate::library_ui::thousands(n));
                    }
                    if self.multi.undo_after && self.multi.jobs.is_empty() {
                        self.multi.undo_after = false;
                        self.undo_turn();
                    }
                }
            }
        }
    }

    /// One saved block: the photos take their new settings (shown preview stays until the new one is rendered).
    fn multi_block(&mut self, changes: &[(Change, crate::sidecar::Meta)]) {
        if changes.is_empty() {
            return;
        }
        let pos: HashMap<i64, usize> = self.entries.iter().enumerate().map(|(i, e)| (e.image, i)).collect();
        let cur = self.current.as_ref().map(|c| c.index);
        let mut dropped = HashSet::new();
        for (c, meta) in changes {
            let Some(&i) = pos.get(&c.image) else { continue };
            if self.entries[i].copy != c.copy {
                continue;
            }
            let opened = self.lib.rating_pending(c.image);
            let e = &mut self.entries[i];
            let view = e.settings.view;
            e.settings = Settings { view, ..c.after.clone() };
            e.loaded = true;
            e.stored = true;
            if !opened {
                e.meta = meta.clone();
            }
            self.saved.insert(c.image, (c.after.clone(), self.entries[i].meta.clone()));
            if let Some((ci, s)) = self.committed.as_mut() {
                if *ci == i {
                    *s = c.after.clone();
                }
            }
            if cur == Some(i) {
                // the active photo itself changed (undo): its state counts as the new base
                self.multi_base = None;
            } else {
                dropped.insert(i);
            }
            self.queue_thumbnail(i);
        }
        if !dropped.is_empty() {
            self.cache.retain(|(ci, _)| !dropped.contains(ci));
        }
        self.compute_thumbnails();
    }

    /// Mark a photo for new previews (virtual copies share the file preview, they are left out).
    pub(crate) fn queue_thumbnail(&mut self, i: usize) {
        let e = &self.entries[i];
        if e.copy.is_none() && e.file > 0 {
            self.thumbnails_pending.insert(e.image);
        }
    }

    /// Re-render the previews of photos whose settings changed, a few at a time in the background: visible cells first,
    /// the rest one part after the other (the next part starts when one is done). Until then a cell keeps its old
    /// preview. A photo that changes again while its preview is being rendered is rendered anew.
    pub(crate) fn compute_thumbnails(&mut self) {
        if !self.thumbnails_running.is_empty() {
            if !self.thumbnails_running.iter().any(|b| self.thumbnails_pending.contains(b)) {
                return;
            }
            // stop the running part, its photos come again
            self.thumbnails_valid.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let back = std::mem::take(&mut self.thumbnails_running);
            self.thumbnails_pending.extend(back);
        }
        if self.thumbnails_pending.is_empty() {
            return;
        }
        let mut chunk: Vec<i64> = self.lib.on_screen.iter().filter(|b| self.thumbnails_pending.contains(b)).take(THUMB_CHUNK).copied().collect();
        if chunk.is_empty() {
            chunk = self.thumbnails_pending.iter().take(THUMB_CHUNK).copied().collect();
        }
        // a small edit (few photos) also renews the large preview of photos on other drives, as before
        let large = self.thumbnails_pending.len() <= RIGHT_AWAY_MAX;
        for b in &chunk {
            self.thumbnails_pending.remove(b);
        }
        let images: Vec<_> = chunk
            .iter()
            .filter_map(|b| self.entries.iter().find(|e| e.image == *b))
            .map(|e| (e.image, e.file, e.path.clone(), Settings { view: develop::View::Developed, ..e.settings.clone() }))
            .collect();
        self.thumbnails_running.extend(images.iter().map(|x| x.0));
        if images.is_empty() {
            // photos no longer in the list: next part
            return self.compute_thumbnails();
        }
        let key = self.thumbnails_valid.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        self.worker.send(crate::jobs::Job::Thumbnails { images, key, valid: self.thumbnails_valid.clone(), large });
    }

    /// Previews still to be re-rendered after edits of several photos.
    pub(crate) fn thumbnails_waiting(&self) -> usize {
        self.thumbnails_pending.len() + self.thumbnails_running.len()
    }

    /// Selected images other than the active one that can be edited.
    fn multi_targets(&self, cur: usize) -> Vec<usize> {
        let grid: HashSet<i64> = self.lib.selection().into_iter().collect();
        (0..self.entries.len())
            .filter(|&i| i != cur && (self.entries[i].selected || grid.contains(&self.entries[i].image)))
            // by file name only: the exact check opens each file (thousands on a network drive) – it runs in the background
            .filter(|&i| crate::image_kind::from_extension(self.entries[i].path.extension().and_then(|e| e.to_str()).unwrap_or("")).editable() && self.entries[i].state == 0)
            .collect()
    }

    /// Is the band "Selection: n photos" shown? (grid, several photos selected, an editing tool)
    pub(crate) fn multi_band_shown(&self) -> bool {
        self.in_grid() && !self.in_people() && self.tool_rail_active().editing() && self.selection_count() > 1 && self.current.is_some()
    }

    /// Place of the band (below the histogram, at the top for tools without one): fixed height in the list. When the
    /// list is scrolled so far that the place leaves the view, the band stays pinned at the top of the visible part –
    /// the button never has to be looked for.
    pub(crate) fn multi_band_slot(&mut self, ui: &mut egui::Ui) {
        if !self.multi_band_shown() {
            return;
        }
        let w = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(egui::vec2(w, BAND_H), egui::Sense::hover());
        ui.add_space(4.0);
        let clip = ui.clip_rect();
        let n = self.selection_count();
        if rect.top() >= clip.top() - 0.5 {
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::top_down(egui::Align::Min)));
            self.multi_band(&mut child, n);
        } else {
            let pinned = egui::Rect::from_min_size(egui::pos2(rect.left(), clip.top()), rect.size());
            let ctx = ui.ctx().clone();
            egui::Area::new(egui::Id::unique("multi-band-pinned")).order(egui::Order::Middle).fixed_pos(pinned.min).constrain(false).show(&ctx, |ui| {
                ui.set_clip_rect(clip);
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(pinned).layout(egui::Layout::top_down(egui::Align::Min)));
                self.multi_band(&mut child, n);
            });
        }
    }

    /// The band: number of photos and way of applying; what is not applied yet and the button – while applying, the
    /// progress with a cancel button in the same place (same height).
    fn multi_band(&mut self, ui: &mut egui::Ui, n: usize) {
        let rect = ui.max_rect();
        ui.painter().rect(rect, 2.0, BAND_FILL, egui::Stroke::new(1.0, BAND_STROKE), egui::StrokeKind::Inside);
        let inner = rect.shrink2(egui::vec2(8.0, 6.0));
        let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Min)));
        let ui = &mut ui;
        ui.spacing_mut().item_spacing.y = 5.0;
        let width = inner.width();
        let others = n.saturating_sub(1);
        let mut mode = self.multi.mode();
        // row 1: number of photos, way of applying
        ui.allocate_ui_with_layout(egui::vec2(width, 20.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            let r = ui.add(egui::Label::new(egui::RichText::new(crate::trf!("Auswahl: {n} Fotos", n = crate::library_ui::thousands(n))).small().color(lr::TEXT)).truncate());
            lr::hover_tip(ui, &r, crate::tr("Die Regler ändern nur das aktive Foto. „Anwenden“ überträgt die Änderungen danach auf die übrigen ausgewählten Fotos."));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let busy = self.multi.progress().is_some();
                let resp = ui.add_enabled_ui(!busy, |ui| lr::segmented_pad(ui, &mut mode, &[(Mode::Relative, "Relativ"), (Mode::Equal, "Gleichsetzen")], 6.0));
                lr::hover_tip(ui, &resp.response, crate::tr("Relativ: gleiche Änderung, Unterschiede bleiben. Gleichsetzen: alle bekommen den Wert des aktiven Fotos. Beispiel: drei Fotos mit −0,3 / 0 / +1,0 EV, Belichtung am aktiven Foto +0,5 → relativ +0,2 / +0,5 / +1,5, gleichgesetzt +0,5 / +0,5 / +0,5. Auswahlen wie der Farbmodus werden immer übernommen."));
            });
        });
        if mode != self.multi.mode() {
            self.multi.set_mode(mode);
        }
        // rows 2 and 3: progress and cancel, or what is waiting and the button
        if let Some((done, total)) = self.multi.progress() {
            ui.add(egui::Label::new(egui::RichText::new(crate::trf!("{k} von {n} gespeichert", k = crate::library_ui::thousands(done), n = crate::library_ui::thousands(total))).small().color(BAND_ACCENT)).truncate());
            ui.allocate_ui_with_layout(egui::vec2(width, 22.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = if self.multi.undo_after { crate::tr("Wird abgebrochen …") } else { crate::tr("Abbrechen") };
                if ui.add_enabled(!self.multi.undo_after, egui::Button::new(label).min_size(egui::vec2(0.0, 22.0))).clicked() {
                    self.multi_cancel();
                    self.status = crate::tr("Wird abgebrochen …").to_string();
                }
                ui.add(egui::ProgressBar::new(done as f32 / total.max(1) as f32).desired_height(6.0).desired_width(ui.available_width() - 6.0));
            });
            return;
        }
        let change = self.multi_change();
        let list = change.as_ref().map(|(_, b, now)| describe(b, now)).unwrap_or_default();
        let (text, color) = match list.len() {
            0 => (crate::tr("Keine Änderung am aktiven Foto").to_string(), lr::TEXT_DIM),
            1 => (crate::trf!("1 Änderung noch nicht angewendet: {a}", a = list[0]), BAND_ACCENT),
            m => (crate::trf!("{m} Änderungen noch nicht angewendet", m = m), BAND_ACCENT),
        };
        let r = ui.add(egui::Label::new(egui::RichText::new(text).small().color(color)).truncate());
        if !list.is_empty() {
            lr::hover_tip(ui, &r, &list.join("\n"));
        }
        let ok = change.is_some() && others > 0;
        let fill = if ok { BAND_BUTTON } else { lr::BUTTON_FILL };
        let b = ui.add_enabled(ok, egui::Button::new(egui::RichText::new(crate::trf!("Auf {n} Fotos anwenden", n = crate::library_ui::thousands(others))).color(if ok { egui::Color32::WHITE } else { lr::TEXT_DIM })).fill(fill).min_size(egui::vec2(width, 22.0)));
        let why = if ok {
            crate::trf!("Überträgt {a} auf die übrigen ausgewählten Fotos. Läuft im Hintergrund; ⌘Z nimmt es in einem Schritt zurück.", a = list.join(", "))
        } else {
            crate::tr("Erst am aktiven Foto etwas ändern, dann auf die Auswahl anwenden.").to_string()
        };
        lr::hover_tip(ui, &b, &why);
        if b.clicked() {
            self.multi_apply_request();
        }
    }

    /// The question before applying to many photos (only while open).
    fn multi_confirm_dialog(&mut self, ctx: &egui::Context) {
        if !self.multi.confirm {
            return;
        }
        let Some((cur, base, now)) = self.multi_change() else {
            self.multi.confirm = false;
            return;
        };
        let n = crate::library_ui::thousands(self.multi_targets(cur).len());
        let list = describe(&base, &now).join(", ");
        let how = if self.multi.mode() == Mode::Relative { crate::tr("relativ (gleiche Änderung)") } else { crate::tr("gleichgesetzt (gleicher Wert)") };
        let title = crate::trf!("Auf {n} Fotos anwenden?", n = n);
        let a = crate::dialog::show(
            ctx,
            "mehrfach-anwenden",
            &title,
            560.0,
            |ui| {
                crate::dialog::text(ui, &crate::trf!("Geändert am aktiven Foto: {a}", a = list));
                crate::dialog::text(ui, &crate::trf!("Wird {how} auf {n} weitere Fotos übertragen.", how = how, n = n));
                crate::dialog::hint(ui, crate::tr("Läuft im Hintergrund mit Fortschritt und lässt sich abbrechen; ⌘Z nimmt alles in einem Schritt zurück. Die Vorschauen folgen nach und nach."));
            },
            crate::dialog::Buttons::confirm(crate::tr("Anwenden"), true),
        );
        match a {
            crate::dialog::Answer::Open => {}
            crate::dialog::Answer::Primary => {
                self.multi.confirm = false;
                self.multi_apply_now();
            }
            crate::dialog::Answer::Cancel => self.multi.confirm = false,
        }
    }
}

#[cfg(test)]
mod preview_tests {
    use crate::*;
    use eframe::App as _;
    use eframe::egui::{self, Pos2, Rect, Vec2};

    fn brightness(img: &image::RgbImage) -> f32 {
        img.pixels().map(|p| p.0.iter().map(|&v| v as f32).sum::<f32>() / 3.0).sum::<f32>() / img.pixels().len().max(1) as f32
    }

    /// In the grid a change on the active photo: the grid preview shows it (darker after −2 EV).
    #[test]
    fn grid_preview_follows_the_edit() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        let folder = std::env::temp_dir().join(format!("graineo-rastervorschau-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        image::RgbImage::from_pixel(600, 400, image::Rgb([150, 150, 150])).save(folder.join("a.png")).unwrap();
        image::RgbImage::from_pixel(600, 400, image::Rgb([150, 150, 150])).save(folder.join("b.png")).unwrap();
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let _app_lock = crate::ui_test::app_lock();
        let mut a = App::new(&cc);
        a.open_folder_inner(folder);
        a.to_grid();
        let first = a.entries[0].image;
        a.lib.set_active(first);
        a.choose_tool(crate::workspace::ToolTab::Edit);
        let mut time = 10.0;
        // `pressed`: mouse button down (as when dragging a slider) or released
        let mut frame = |a: &mut App, events: Vec<egui::Event>| {
            time += 0.2;
            let mut input = egui::RawInput { time: Some(time), screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 1000.0))), events, ..Default::default() };
            a.raw_input_hook(&ctx, &mut input);
            let _ = crate::ui_test::run(&ctx, input, |ui| a.ui(ui, &mut eframe::Frame::_new_kittest()));
            a.handle_results(&ctx);
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        let deadline = std::time::Instant::now() + crate::ui_test::WAIT;
        while a.current.as_ref().is_none_or(|c| c.texture.is_none()) {
            frame(&mut a, vec![]);
            assert!(std::time::Instant::now() < deadline, "active photo is not loaded in the grid");
        }
        assert!(a.in_grid());
        let i = a.current.as_ref().unwrap().index;
        let file = a.entries[i].file;
        // "drag" a slider: mouse pressed (at an empty spot top left), change, let it render
        let p = Pos2::new(5.0, 990.0);
        frame(&mut a, vec![egui::Event::PointerMoved(p), egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() }]);
        a.entries[i].settings.exposure = -2.0;
        let deadline = std::time::Instant::now() + crate::ui_test::WAIT;
        while a.rendered_for.as_ref().is_none_or(|(_, s)| s.exposure != -2.0) {
            frame(&mut a, vec![]);
            assert!(std::time::Instant::now() < deadline, "no computation after the change");
        }
        // the grid already shows the change while dragging
        let (d, brightness_value) = a.lib.last_stored.expect("no preview while dragging");
        assert!(d == file && brightness_value < 100.0, "while dragging: brightness {brightness_value}");
        a.lib.last_stored = None;
        // it is saved on release
        frame(&mut a, vec![egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() }]);
        frame(&mut a, vec![]);
        let (d, brightness_value) = a.lib.last_stored.expect("no new preview");
        assert_eq!(d, file);
        assert!(brightness_value < 100.0, "preview does not show the change: brightness {brightness_value}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn relative() {
        let a = json!({"exposure": 0.0, "hsl": [[0.0, 0.1, 0.0]], "monochrome": false, "raw": {"look": ""}});
        let n = json!({"exposure": 0.5, "hsl": [[0.0, 0.3, 0.0]], "monochrome": true, "raw": {"look": "x"}});
        let mut z = json!({"exposure": 1.0, "hsl": [[0.2, 0.0, 0.0]], "monochrome": false, "raw": {"look": "y"}});
        transfer(&a, &n, &mut z);
        assert_eq!(z["exposure"].as_f64(), Some(1.5));
        assert!((z["hsl"][0][1].as_f64().unwrap() - 0.2).abs() < 1e-9);
        assert_eq!(z["hsl"][0][0].as_f64(), Some(0.2));
        assert_eq!(z["monochrome"], json!(true));
        assert_eq!(z["raw"]["look"], json!("x"));
    }

    #[test]
    fn relative_and_equal_as_in_the_tooltip_example() {
        // three photos at −0.3 / 0 / +1.0 EV, the active one (0) gets +0.5
        let base = Settings::default();
        let now = Settings { exposure: 0.5, monochrome: true, ..Settings::default() };
        let photos = [-0.3, 0.0, 1.0].map(|ev| Settings { exposure: ev, ..Settings::default() });
        let rel = step(&base, &now, Mode::Relative);
        let eq = step(&base, &now, Mode::Equal);
        let r: Vec<f64> = photos.iter().map(|p| rel(p).exposure).collect();
        let e: Vec<f64> = photos.iter().map(|p| eq(p).exposure).collect();
        assert!(r.iter().zip([0.2, 0.5, 1.5]).all(|(a, b)| (a - b).abs() < 1e-9), "{r:?}");
        assert!(e.iter().all(|v| (v - 0.5).abs() < 1e-9), "{e:?}");
        // a choice is copied in both ways; what did not change stays
        let other = Settings { exposure: 1.0, contrast: 0.4, ..Settings::default() };
        assert!(rel(&other).monochrome && eq(&other).monochrome);
        assert_eq!(eq(&other).contrast, 0.4);
    }

    #[test]
    fn describe_names_the_changes() {
        let base = Settings::default();
        let now = Settings { exposure: 0.3, monochrome: true, contrast: 0.2, ..Settings::default() };
        let d = describe(&base, &now);
        assert_eq!(d[0], "Belichtung +0,30");
        assert!(d.contains(&"Schwarzweiß".to_string()), "{d:?}");
        assert!(d.contains(&"Tonwert".to_string()), "contrast is in the group Tonwert: {d:?}");
        assert!(describe(&base, &base).is_empty());
    }

    #[test]
    fn back_to_default_removes_the_key() {
        // binning Off → Auto: the field is left out when automatic, the selection follows
        let a = json!({"exposure": 0.0, "binning": "off"});
        let n = json!({"exposure": 0.0});
        let mut z = json!({"exposure": 1.0, "binning": "off"});
        transfer(&a, &n, &mut z);
        assert!(z.get("binning").is_none());
        let mut z2 = json!({"exposure": 1.0});
        transfer(&n, &a, &mut z2);
        assert_eq!(z2["binning"], json!("off"));
    }
}
