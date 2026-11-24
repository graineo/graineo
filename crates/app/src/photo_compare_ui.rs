//! Drawing, keys and full resolution of the photo comparison (state and rules: `photo_compare.rs`, hints and series
//! decisions: `library_ui/library_compare.rs`). The comparison is a view of the centre: in the library it replaces
//! the grid (the filmstrip appears below it), in Develop the single image (the panel on the right edits the focused
//! field, the zoom group below the image acts on all fields).
//!
//! Full quality ("Vergleich muss IMMER voll sein"): every visible field shows its
//! photo developed with its own settings like the single image (`Job::HoverRender` of the opened photo, 2.8 MP),
//! never the library preview for good; until then the field says "wird berechnet". Zoomed beyond that, every visible
//! field gets crops of the visible part at full resolution once the view rests. Order: the focused field first, then
//! the others; only visible fields, one job at a time (the crops' gate), nothing for pages not shown, and what was
//! asked for a field no longer shown is cancelled. The photos are opened one after the other through the app's
//! cache of preloaded photos (`App::cache`): the pool holds the focused photo and the next ones still missing their
//! development or crop (at most four, two in the operating mode "Saving"), so eight fields never hold eight opened
//! photos. An original that cannot be reached keeps the library preview with the mark "Original offline". In
//! Develop the focused field is the photo on screen (live).

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use develop::geometry::Frame;
use develop::Settings;
use eframe::egui::{self, Color32, Pos2, Rect, TextureHandle, Vec2};

use crate::library_ui::CompareRequest;
use crate::photo_compare::{self as pc, Arrange, Cmd, Compare, Decision, Fit, Mode, RefView, Tournament};
use crate::{lr, App, Tab};

/// Gap between the fields (shown whole).
const GAP: f32 = 18.0;
/// Joint between the fields filled by their photos.
const FILL_GAP: f32 = 4.0;
/// Height of the foot of a field card (stars, hints, ✓ ✗ in one row).
const FOOT_H: f32 = 26.0;
/// Ground of a field card (the foot and the frame around the photo).
const CARD: Color32 = Color32::from_gray(0x1c);

/// The app's part of the comparison: crops at full resolution and the state of the view.
#[derive(Default)]
pub struct CompareView {
    /// crops at full resolution: image → (settings, region in the source image, texture)
    details: HashMap<i64, (Settings, [usize; 4], TextureHandle)>,
    /// crop on its way: image, settings, region, time sent
    pending: Option<(i64, Settings, [usize; 4], f64)>,
    /// no new crop before this time (after a failure)
    blocked_until: f64,
    /// generation of the comparison's crop jobs (a change of the photo in Develop does not drop them)
    generation: Arc<AtomicUsize>,
    /// indices of the photos the comparison keeps opened (protected in the cache)
    pub pool: Vec<usize>,
    /// last change of the view (crops only once it rests)
    changed_at: f64,
    /// shared centre of the view (0..1 of the photos) and the pan written to the app last frame
    center: Vec2,
    last_pan: Vec2,
    /// zoom and centre before "space held"
    space: Option<(f32, Vec2)>,
    /// drag on a single field (⇧-drag): its photo
    drag_single: Option<i64>,
    /// first frame with a field shown at 100 % (per image), for the log of the time to full resolution
    waiting_since: HashMap<i64, f64>,
    /// blink: start of the phase
    blink_start: f64,
    /// a test mode to apply once the comparison has its photos
    test_mode: Option<String>,
    /// measuring: start of the drag (0..1 of the photo), sharpness per photo (100 = sharpest), the sharpest
    measure_start: Option<Pos2>,
    measure_values: HashMap<i64, f32>,
    measure_best: Option<i64>,
    /// peaking, clipping and difference over the fields
    overlays: Overlays,
    /// the arrangement as last saved
    arrange_saved: Option<Arrange>,
    fit_saved: Option<Fit>,
    /// opening a photo for full resolution was refused for lack of memory (the label says so)
    pool_refused: bool,
    /// arrangements that give exactly the fields shown now (greyed in the menu, skipped by G)
    same_layout: Vec<Arrange>,
    /// fields of "Alle" that fit the area of the last frame (0: not known yet)
    all_cap: usize,
    /// fitted: the scale of the focused field in percent, once its photo is known (`None`: still being computed –
    /// the zoom field shows "–" instead of a wrong "100 %", QA 1.5.0-test.7)
    pub(crate) fit_pct: Option<f32>,
    /// Develop: the photo open when the comparison started (it comes back on closing without a choice)
    back_to: Option<i64>,
    /// field pictures developed with the photo's settings: image → (entry settings they were made for, texture of
    /// the source image, its frame, source size)
    developed: HashMap<i64, (Settings, TextureHandle, Frame, [usize; 2])>,
    /// development on its way: image, entry settings, time sent
    develop_pending: Option<(i64, Settings, f64)>,
    /// exact base on its way for a field whose fast preview is too rough to be shown (`preview_hold.rs`): image,
    /// cancel flag, time sent
    base_pending: Option<(i64, Arc<std::sync::atomic::AtomicBool>, f64)>,
    /// photos whose exact base failed: their fields are developed from the fast preview (the best picture there is)
    base_failed: std::collections::HashSet<i64>,
    /// what each visible field shows (the marks say so)
    states: HashMap<i64, FieldState>,
    /// Lightroom start values of photos without own settings (as Develop would open them)
    start: HashMap<i64, Settings>,
    /// the photos shown last frame (a change cancels what was asked for the others)
    shown_last: Vec<i64>,
    /// the photo the comparison is opening (its preload is cancelled once it is no longer shown)
    opening: Option<usize>,
    /// when the fields shown now appeared, and whether "all fields full" was logged for them (time to full quality)
    shown_at: f64,
    full_logged: bool,
}

/// What a field shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FieldState {
    /// developed with its settings (or the live photo in Develop, or a format without development)
    Full,
    /// the library preview until the development is there ("wird berechnet")
    Computing,
    /// the original cannot be reached: the library preview stays ("Original offline")
    Offline,
    /// opening was refused for lack of memory: the library preview stays for now
    Memory,
    /// developed from the fast preview because the exact base of the photo failed (more noise than in Develop's
    /// exact view; the mark says so)
    Fast,
}

/// Field picture: a preview of the library (upright, cropped) or the photo on screen in Develop (source image with
/// its frame).
enum Pic {
    Plain(TextureHandle),
    Frame(TextureHandle, Frame, [f32; 4]),
}

/// One field to draw.
struct Field {
    image: i64,
    rect: Rect,
    aspect: f32,
    pic: Option<Pic>,
    /// frame of the opened photo (for crops at full resolution)
    frame: Option<(Frame, [usize; 2])>,
    /// preview width in pixels (full resolution is needed beyond it)
    preview_px: f32,
    /// width of the photo in pixels (for "100 %")
    pixels: f32,
}

impl App {
    /// Comparison open in Develop (the centre shows it instead of the single image).
    pub(crate) fn compare_in_develop(&self) -> bool {
        self.lib.compare_open() && self.tab == Tab::Develop && !self.darkroom_active()
    }

    /// Comparison shown in the centre (library or Develop).
    pub(crate) fn compare_shown(&self) -> bool {
        self.lib.compare_open() && (self.in_grid() || self.compare_in_develop())
    }

    /// Head of the right panel while comparing in Develop: the photo edited and its field.
    pub(crate) fn compare_panel_head(&self) -> Option<String> {
        if !self.compare_in_develop() {
            return None;
        }
        let c = self.lib.compare.as_ref()?;
        let f = c.focus?;
        let k = c.field_index(f)?;
        let name = self.entries.iter().find(|e| e.image == f).map(|e| e.name.clone())?;
        Some(format!("{name} · {}", crate::trf!("Feld {n}", n = k + 1)))
    }

    /// Images selected (capture order) and the active one: the grid's selection in the library, the filmstrip's in
    /// Develop.
    fn compare_selection(&mut self) -> (Vec<i64>, Option<i64>) {
        if self.in_grid() {
            let sel = self.lib.target_images();
            return (self.strip_order(sel), self.lib.active_image());
        }
        // the library's selection becomes the filmstrip's (opening a photo clears the library's)
        for i in self.chosen_entries() {
            self.entries[i].selected = true;
        }
        let cur = self.loading.or(self.current.as_ref().map(|c| c.index)).or(self.not_developable);
        let mut images: Vec<i64> = self.entries.iter().filter(|e| e.selected).map(|e| e.image).collect();
        if let Some(e) = cur.and_then(|i| self.entries.get(i)) {
            if !images.contains(&e.image) {
                images.push(e.image);
            }
        }
        (self.strip_order(images), cur.and_then(|i| self.entries.get(i)).map(|e| e.image))
    }

    /// Images in the order of the filmstrip (fields, numbers and the strip's marks then agree); images not in the
    /// strip (another folder shown) keep their capture order at the end.
    fn strip_order(&self, images: Vec<i64>) -> Vec<i64> {
        let pos: std::collections::HashMap<i64, usize> = self.entries.iter().enumerate().map(|(i, e)| (e.image, i)).collect();
        let mut rest: Vec<i64> = images.iter().copied().filter(|b| !pos.contains_key(b)).collect();
        rest = self.lib.compare_sorted(rest);
        let mut v: Vec<i64> = images.into_iter().filter(|b| pos.contains_key(b)).collect();
        v.sort_by_key(|b| pos[b]);
        v.dedup();
        v.extend(rest);
        v
    }

    fn entry_of(&self, image: i64) -> Option<usize> {
        self.entries.iter().position(|e| e.image == image)
    }

    /// Open the comparison wished for (button, N, context menu, series) in the view shown.
    pub(crate) fn compare_requests(&mut self) {
        let Some(req) = self.lib.take_compare_request() else { return };
        self.compare_open_with(req);
    }

    pub(crate) fn compare_open_with(&mut self, req: CompareRequest) {
        if self.lib.compare_open() && req.stack.is_none() && req.reference.is_none() {
            return;
        }
        if req.stack.is_none() && req.reference.is_none() {
            let (sel, _) = self.compare_selection();
            if sel.len() < 2 {
                self.status = crate::tr("Zum Vergleichen mindestens 2 Fotos auswählen (⌘-Klick)").to_string();
                return;
            }
        }
        // Develop: the photo open now comes back when the comparison is closed without a choice (QA 5, F7)
        self.cmp.back_to = if self.in_grid() { None } else { self.current.as_ref().map(|c| self.entries[c.index].image) };
        // from Develop: the selection of the filmstrip is kept for the library's series command
        self.lib.compare_start(req.stack, req.reference);
        if !self.in_grid() {
            // a series chosen as a whole: its photos into the filmstrip's selection, its cover (or the reference) open
            let images = self.lib.target_images();
            if images.len() > 1 {
                for e in self.entries.iter_mut() {
                    e.selected = images.contains(&e.image);
                }
                let cur = self.current.as_ref().map(|c| self.entries[c.index].image);
                if cur.is_none_or(|c| !images.contains(&c)) {
                    if let Some(i) = req.reference.or(self.lib.active_image()).and_then(|b| self.entry_of(b)) {
                        self.select(i, true);
                    }
                }
            }
            if let Some(i) = req.reference.and_then(|b| self.entry_of(b)) {
                self.entries[i].selected = true;
            }
        }
        self.zoom = 1.0;
        self.pan = Vec2::ZERO;
        self.compare = false;
        self.show_before = false;
        self.cmp.center = Vec2::splat(0.5);
        self.cmp.last_pan = Vec2::ZERO;
        self.cmp.details.clear();
        self.cmp.fit_pct = None;
        self.cmp.test_mode = req.test_mode;
        crate::log("compare: opened");
    }

    /// Close the comparison without a choice (Esc, "Vergleich schließen", the view switch): in Develop the photo that
    /// was open before it comes back (QA 5, F7: the focused field's photo stayed); only "In Entwickeln öffnen" of a
    /// field changes the photo. In the library as [`Self::compare_close`].
    pub(crate) fn compare_close_back(&mut self) {
        let back = self.cmp.back_to.take();
        self.compare_close();
        if !self.in_grid() {
            if let Some(i) = back.and_then(|b| self.entry_of(b)) {
                self.select(i, true);
            }
        }
    }

    /// Leave the comparison: in the library back to the grid, in Develop to the single image (the focused photo).
    pub(crate) fn compare_close(&mut self) {
        self.cmp.back_to = None;
        self.lib.compare_close();
        self.cmp.details.clear();
        self.cmp.pending = None;
        // nothing of the comparison goes on after closing: developments, the photo being opened
        self.cmp.developed.clear();
        self.cmp.develop_pending = None;
        self.cmp.states.clear();
        self.cmp.shown_last.clear();
        if let Some(i) = self.cmp.opening.take() {
            if let Some((_, flag)) = self.prefetch_flags.iter().find(|(n, _)| *n == i) {
                flag.store(true, Ordering::SeqCst);
            }
        }
        self.cmp.pool.clear();
        self.cmp.space = None;
        self.cmp.generation.fetch_add(1, Ordering::SeqCst);
        self.zoom = 1.0;
        self.pan = Vec2::ZERO;
        crate::log("compare: closed");
    }

    /// ⌘Z while the tournament is shown (a duel or its result): the last duel back. `true`: taken back.
    pub(crate) fn compare_undo_duel(&mut self) -> bool {
        if !self.compare_shown() {
            return false;
        }
        match self.lib.compare.as_mut() {
            Some(c) if c.mode == Mode::Tournament || c.ranking_shown() => c.undo_duel(),
            _ => false,
        }
    }

    /// Keys that open the comparison outside of it: N in Develop.
    pub(crate) fn compare_entry_keys(&mut self, ctx: &egui::Context) {
        if !self.lib.compare_open() && !self.in_grid() && !self.darkroom_active() && crate::shortcuts::pressed(ctx, "compare_open") {
            // as N in the library: several photos selected in the filmstrip are compared as they are; otherwise the
            // whole series of the active photo (QA 1.5.0-test.7: on the cover of a collapsed series N only said
            // "mindestens 2 Fotos auswählen")
            if self.compare_selection().0.len() >= 2 {
                self.compare_open_with(CompareRequest::default());
            } else {
                let a = self.lib.stack_menu_command(crate::library_ui::StackMenu::Review);
                self.library_actions(a);
            }
        }
    }

    // ───────────────────────────────────────────── Keys

    /// Keys of the comparison, before those of grid and single image: the events it handles are taken out.
    pub(crate) fn compare_keys(&mut self, ctx: &egui::Context) {
        if !self.compare_shown() || ctx.egui_wants_keyboard_input() {
            return;
        }
        let taken: std::cell::RefCell<Vec<(crate::shortcuts::Mods, egui::Key)>> = Default::default();
        let hit = |id: &str| -> bool {
            if !crate::shortcuts::pressed(ctx, id) {
                return false;
            }
            if let Some(crate::shortcuts::Trigger::KeyPress(m, k)) = crate::shortcuts::find(id).map(|s| s.trigger) {
                taken.borrow_mut().push((m, k));
            }
            true
        };
        let none = crate::shortcuts::Mods::default();
        let key = |k: egui::Key| ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Key { key, pressed: true, modifiers, .. } if *key == k && !modifiers.command && !modifiers.shift && !modifiers.alt)));
        let popup = egui::Popup::is_any_open(ctx);
        let mut cmds: Vec<Cmd> = Vec::new();
        let Some(mut c) = self.lib.compare.take() else { return };
        let tour = c.mode == Mode::Tournament && c.tournament.as_ref().is_some_and(|t| !t.done);
        if hit("cmp_finish") {
            cmds.push(Cmd::Finish);
        }
        // ⌘Z in the tournament: the last duel (otherwise the usual undo) – also on the result shown after the last
        // duel, where the usual undo would take back an older rating instead
        let duel_undo = (c.mode == Mode::Tournament && c.tournament.is_some()) || c.ranking_shown();
        if duel_undo && ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Key { key: egui::Key::Z, pressed: true, modifiers, .. } if modifiers.command && !modifiers.shift))) {
            if c.undo_duel() {
                taken.borrow_mut().push((crate::shortcuts::Mods { cmd: true, ..none }, egui::Key::Z));
            }
        }
        // Esc with a menu of the bar open (arrangement, reference): only the menu closes (QA 1.5.0-test.7: the key went on
        // to the grid / single view and ended the whole comparison)
        if popup && key(egui::Key::Escape) {
            egui::Popup::close_all(ctx);
            taken.borrow_mut().push((none, egui::Key::Escape));
        }
        if !popup && hit("cmp_back") {
            if tour {
                c.mode = Mode::Compare;
            } else if c.ranking_shown() {
                // Esc on the ranking: dropped, nothing set (the comparison stays)
                discard_ranking(&mut c);
            } else {
                cmds.push(Cmd::Close);
            }
        }
        if hit("cmp_mode_compare") {
            c.mode = Mode::Compare;
        }
        if hit("cmp_mode_tournament") {
            start_tournament(&mut c);
        }
        // G skips arrangements that would show the same fields; in the tournament the arrangement is fixed
        if hit("cmp_arrange") && c.mode != Mode::Tournament {
            c.arrange = next_distinct(c.arrange, &self.cmp.same_layout);
        }
        if hit("cmp_coupled") {
            c.coupled = !c.coupled;
        }
        if hit("cmp_align") {
            c.offsets.clear();
        }
        if hit("cmp_fit") {
            c.fit = c.fit.toggled();
        }
        if hit("cmp_faces") {
            c.faces_bar = !c.faces_bar;
        }
        if hit("cmp_focus_point") {
            c.checks.focus_point = !c.checks.focus_point;
        }
        if hit("cmp_peaking") {
            c.checks.peaking = !c.checks.peaking;
        }
        if hit("cmp_clipping") {
            c.checks.clipping = !c.checks.clipping;
        }
        if hit("cmp_exif") {
            c.checks.exif = !c.checks.exif;
        }
        if hit("cmp_measure") {
            c.checks.measure = !c.checks.measure;
            if !c.checks.measure {
                c.measure_rect = None;
            }
        }
        if hit("cmp_reference") {
            if let Some(f) = c.focus {
                c.toggle_reference(f);
            }
        }
        for (id, v) in [("cmp_blink", RefView::Blink), ("cmp_wipe", RefView::Wipe), ("cmp_difference", RefView::Difference)] {
            if hit(id) && c.reference.is_some() {
                c.ref_view = if c.ref_view == v { RefView::SideBySide } else { v };
                self.cmp.blink_start = ctx.input(|i| i.time);
            }
        }
        let zoom = hit("cmp_zoom");
        let zoom200 = hit("cmp_zoom_200");
        if let Some(n) = crate::shortcuts::which(ctx, "cmp_field") {
            if let Some(&img) = c.fields.get(n) {
                cmds.push(Cmd::Focus(img));
            }
            if let Some(crate::shortcuts::Trigger::Shortcuts(m, keys)) = crate::shortcuts::find("cmp_field").map(|s| s.trigger) {
                taken.borrow_mut().push((m, keys[n]));
            }
        }
        if tour {
            let t = c.tournament.as_mut().unwrap();
            let d = if key(egui::Key::ArrowLeft) {
                Some(Decision::Left)
            } else if key(egui::Key::ArrowRight) {
                Some(Decision::Right)
            } else if hit("cmp_tie") {
                Some(Decision::Tie)
            } else if hit("cmp_both_out") {
                Some(Decision::BothOut)
            } else {
                None
            };
            if let Some(d) = d {
                t.decide(d);
            }
        } else if key(egui::Key::ArrowLeft) || key(egui::Key::ArrowRight) {
            let before = c.focus;
            c.step_focus(if key(egui::Key::ArrowLeft) { -1 } else { 1 });
            if let Some(f) = c.focus.filter(|f| Some(*f) != before) {
                cmds.push(Cmd::Focus(f));
            }
        }
        // ↓/↑ no longer go to the next / previous series (to reach another series one
        // leaves the comparison); they are taken so that they do not move the photo behind the comparison
        for k in [egui::Key::ArrowLeft, egui::Key::ArrowRight, egui::Key::ArrowUp, egui::Key::ArrowDown] {
            taken.borrow_mut().push((none, k));
        }
        if hit("cmp_rest") {
            cmds.push(Cmd::Rest);
        }
        if hit("cmp_choose") {
            if let Some(f) = c.focus {
                cmds.push(Cmd::Best(f));
            }
        }
        if hit("cmp_develop") && self.in_grid() {
            if let Some(f) = c.focus {
                cmds.push(Cmd::Develop(f));
            }
        }
        // the space bar belongs to the comparison (held: 100 % at the pointer)
        taken.borrow_mut().push((none, egui::Key::Space));
        let taken = taken.into_inner();
        c.pending.extend(cmds);
        self.lib.compare = Some(c);
        if zoom {
            self.compare_zoom_key(1.0);
        }
        if zoom200 {
            self.compare_zoom_key(2.0);
        }
        ctx.input_mut(|i| {
            i.events.retain(|e| match e {
                egui::Event::Key { key, physical_key, pressed: true, modifiers, .. } => {
                    let k = physical_key.unwrap_or(*key);
                    !taken.iter().any(|(m, t)| (*t == k || *t == *key) && modifiers.command == m.cmd && modifiers.shift == m.shift && modifiers.alt == m.alt)
                }
                _ => true,
            })
        });
        self.compare_commands(ctx);
    }

    /// Z: fit ↔ `factor` × 100 % (at the focus point, a face or the sharpest region of the focused photo).
    fn compare_zoom_key(&mut self, factor: f32) {
        // size of the photo not known yet (opening): about 100 % of a 24 MP photo until it is
        let one = if self.one_to_one > 1.0 { self.one_to_one } else { 4.0 };
        if self.zoom > 1.01 && (factor == 1.0 || (self.zoom - one * factor).abs() < 0.01) {
            self.zoom = 1.0;
            self.cmp.center = Vec2::splat(0.5);
        } else {
            self.zoom = (one * factor).max(1.0);
            let target = self.lib.compare_focus().and_then(|f| self.lib.compare_hint(f)).and_then(|h| h.zoom_target());
            self.cmp.center = target.map_or(self.cmp.center, |p| p.to_vec2());
        }
        self.cmp.changed_at = self.now();
    }

    /// Commands of buttons and keys, after drawing.
    fn compare_commands(&mut self, ctx: &egui::Context) {
        let cmds = match self.lib.compare.as_mut() {
            Some(c) => std::mem::take(&mut c.pending),
            None => return,
        };
        let mut actions = Vec::new();
        for cmd in cmds {
            match cmd {
                Cmd::Best(b) => self.lib.compare_choose(b, &mut actions),
                Cmd::Rest => self.lib.compare_rest(&mut actions),
                Cmd::Finish => {
                    if !self.lib.compare_finish(&mut actions) {
                        self.compare_close();
                    } else {
                        self.compare_follow_series();
                    }
                }
                Cmd::Close => {
                    self.compare_close_back();
                }
                Cmd::Focus(b) => self.compare_focus_on(b),
                Cmd::Develop(b) => {
                    self.compare_close();
                    self.lib.set_active(b);
                    self.to_single_image();
                }
                Cmd::Remove(b) => self.compare_deselect(b),
                Cmd::ShowInGrid(b) => {
                    self.compare_close();
                    if !self.in_grid() {
                        self.to_grid();
                    }
                    self.lib.set_active(b);
                }
                Cmd::Stars(b, s) => {
                    let new = self.lib.rate_images(&[b], Some(s), None, None);
                    self.adopt_values(&new);
                }
                Cmd::Flag(b, f) => {
                    let new = self.lib.rate_images(&[b], None, Some(f), None);
                    self.adopt_values(&new);
                }
                Cmd::Adopt => {
                    let t = self.lib.compare.as_mut().and_then(|c| c.tournament.take());
                    if let Some(t) = t {
                        self.lib.compare_adopt(t.places(), &t.entrants, &mut actions);
                    }
                    if let Some(c) = self.lib.compare.as_mut() {
                        c.leave_ranking();
                        c.mode = Mode::Compare;
                    }
                }
            }
        }
        let _ = ctx;
        self.library_actions(actions);
    }

    /// Another series was chosen: in Develop its photos become the filmstrip's selection.
    fn compare_follow_series(&mut self) {
        self.zoom = 1.0;
        self.cmp.center = Vec2::splat(0.5);
        self.cmp.details.clear();
        if self.in_grid() {
            return;
        }
        let images = self.lib.target_images();
        for e in self.entries.iter_mut() {
            e.selected = images.contains(&e.image);
        }
        if let Some(i) = self.lib.active_image().and_then(|b| self.entry_of(b)) {
            self.select(i, true);
        }
    }

    /// Focus a field: the library makes it active (the info panel shows it), Develop opens it (the cache usually holds
    /// it already).
    fn compare_focus_on(&mut self, image: i64) {
        if let Some(c) = self.lib.compare.as_mut() {
            c.focus = Some(image);
        }
        if self.in_grid() {
            self.lib.set_active_keep(image);
        } else if let Some(i) = self.entry_of(image) {
            let t = self.now();
            self.select(i, true);
            crate::log(&format!("compare: focus {} ({})", self.entries[i].name, if self.loading == Some(i) { "opening".to_string() } else { format!("from the cache, {:.0} ms", (self.now() - t) * 1000.0) }));
        }
    }

    /// Take a photo out of the comparison (and out of the selection).
    fn compare_deselect(&mut self, image: i64) {
        self.lib.compare_select(image, false);
        if let Some(i) = self.entry_of(image) {
            self.entries[i].selected = false;
        }
        if let Some(c) = self.lib.compare.as_mut() {
            if c.reference == Some(image) {
                c.reference = None;
            }
        }
        // Develop: the photo on screen left – the next field opens
        if !self.in_grid() && self.current.as_ref().is_some_and(|c| self.entries[c.index].image == image) {
            let next = self.lib.compare.as_ref().and_then(|c| c.fields.iter().copied().find(|&x| x != image));
            if let Some(n) = next {
                self.compare_focus_on(n);
            }
        }
    }

    // ───────────────────────────────────────────── Filmstrip

    /// A click in the filmstrip while comparing. `true`: handled.
    pub(crate) fn compare_strip_click(&mut self, i: usize, cmd: bool, shift: bool) -> bool {
        if !self.compare_shown() || shift {
            return false;
        }
        let image = self.entries[i].image;
        let (sel, _) = self.compare_selection();
        let selected = sel.contains(&image);
        if cmd {
            // ⌘-click: into the comparison or out of it
            if selected {
                if sel.len() > 1 {
                    self.compare_deselect(image);
                }
            } else {
                self.lib.compare_select(image, true);
                self.entries[i].selected = true;
            }
            return true;
        }
        let Some(mut c) = self.lib.compare.take() else { return false };
        if c.mode == Mode::Tournament {
            self.lib.compare = Some(c);
            return true;
        }
        if !selected {
            // a photo not selected becomes the candidate: it takes the place of the focused photo in the selection
            if let Some(f) = c.focus.filter(|f| Some(*f) != c.reference && sel.len() > 1) {
                self.lib.compare_select(f, false);
                if let Some(k) = self.entry_of(f) {
                    self.entries[k].selected = false;
                }
            }
            self.lib.compare_select(image, true);
            self.entries[i].selected = true;
        }
        c.swap_into_focus(image);
        self.lib.compare = Some(c);
        self.compare_focus_on(image);
        true
    }

    /// Marks of the comparison on a filmstrip cell: number and white top edge = in the comparison, golden bottom edge =
    /// reference, dimmed = out of the tournament, L/R = the duel.
    pub(crate) fn compare_strip_mark(&self, painter: &egui::Painter, r: Rect, image: i64) {
        let Some(c) = self.lib.compare.as_ref().filter(|_| self.compare_shown()) else { return };
        if let Some(t) = c.tournament.as_ref().filter(|_| c.mode == Mode::Tournament) {
            if t.is_out(image) {
                painter.rect_filled(r, 0.0, Color32::from_black_alpha(150));
            }
            if let Some((a, b)) = t.pair() {
                let side = if a == image { Some("L") } else if b == image { Some("R") } else { None };
                if let Some(s) = side {
                    let at = r.left_top() + Vec2::new(4.0, 4.0);
                    let tr = Rect::from_min_size(at, Vec2::new(14.0, 14.0));
                    painter.rect_filled(tr, 0.0, Color32::from_gray(0xf0));
                    painter.text(tr.center(), egui::Align2::CENTER_CENTER, s, lr::semibold(10.0), Color32::BLACK);
                }
            }
        } else if c.field_index(image).is_some() {
            // a photo with a field: a white bar along the top (the selection frame marks every compared photo). No digit:
            // a lone "1" on a cell said nothing (found in 1.5.0-test.5), and the cell's number is its place in the
            // series (library_series.rs)
            painter.rect_filled(Rect::from_min_max(r.left_top(), egui::pos2(r.right(), r.top() + 3.0)), 0.0, Color32::WHITE);
        }
        if c.reference == Some(image) {
            painter.rect_filled(Rect::from_min_max(egui::pos2(r.left(), r.bottom() - 2.0), r.max), 0.0, lr::GOLD);
        }
    }

    /// The filmstrip header while comparing: how many photos have a field.
    pub(crate) fn compare_strip_note(&self) -> Option<String> {
        let c = self.lib.compare.as_ref().filter(|_| self.compare_shown())?;
        Some(crate::trf!("· {a} ausgewählt · {b} im Vergleich", a = c.images.len(), b = c.fields.len()))
    }

    // ───────────────────────────────────────────── Centre

    /// The comparison in the centre (library or Develop).
    pub(crate) fn compare_center(&mut self, ui: &mut egui::Ui) {
        let library = self.in_grid();
        self.lib.compare_poll();
        let (sel, active) = self.compare_selection();
        // a photo taken in from another series opens that series in the filmstrip as well
        self.lib.compare_hold(&sel);
        // what the library says about the comparison, read before it is taken out for drawing
        let head = self.lib.compare_header();
        let Some(mut c) = self.lib.compare.take() else { return };
        if self.cmp.all_cap > 0 {
            c.all_max = self.cmp.all_cap;
        }
        if c.show_ranking_if_done() {
            // place 1 focused (Develop opens it)
            if let Some(f) = c.focus {
                c.pending.push(Cmd::Focus(f));
            }
        }
        c.follow(sel, active);
        // Develop: the focused field is the photo on screen
        if !library {
            if let Some(cur) = self.current.as_ref().map(|x| self.entries[x.index].image).or(active) {
                if c.images.contains(&cur) {
                    if !c.fields.contains(&cur) && c.mode != Mode::Tournament {
                        c.swap_into_focus(cur);
                    }
                    c.focus = Some(cur);
                }
            }
        } else if let Some(f) = c.focus {
            if self.lib.active_image() != Some(f) {
                self.lib.set_active_keep(f);
            }
        }
        // test aid (own data folder only): GRAINEO_COMPARE_SELECT=<n> puts the first n photos of the strip into the comparison
        if let Some(n) = crate::env::var_os("GRAINEO_DATA_DIR").and_then(|_| crate::env::var("GRAINEO_COMPARE_SELECT").ok()).and_then(|v| v.parse::<usize>().ok()) {
            static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if !self.entries.is_empty() && !DONE.swap(true, Ordering::Relaxed) {
                let images: Vec<i64> = self.entries.iter().take(n).map(|e| e.image).collect();
                for (i, e) in self.entries.iter_mut().enumerate() {
                    e.selected = i < n;
                }
                if let Some(&a) = images.first() {
                    self.lib.set_selection(a, &images);
                }
                c.stack = None;
            }
        }
        // test aid (own data folder only): GRAINEO_COMPARE_TEST="...;close=<s>" closes the comparison <s> seconds after
        // the start (measuring the work left after closing)
        if let Some(at) = crate::env::var_os("GRAINEO_DATA_DIR")
            .and_then(|_| crate::env::var("GRAINEO_COMPARE_TEST").ok())
            .and_then(|spec| spec.split(';').find_map(|p| p.strip_prefix("close=").and_then(|v| v.parse::<f64>().ok())))
        {
            static CLOSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if ui.input(|i| i.time) > at && !CLOSED.swap(true, Ordering::Relaxed) {
                crate::log("compare test: closing");
                c.pending.push(Cmd::Close);
            }
        }
        // test aid (own data folder only): GRAINEO_COMPARE_TEST="ref=blink|wipe|diff;check=peaking,clipping,focus,exif;
        // tour=<decisions L/R>;measure=x0:y0:x1:y1" – once the comparison has its photos (screenshots of V2/V3)
        if let Some(spec) = crate::env::var_os("GRAINEO_DATA_DIR").and_then(|_| crate::env::var("GRAINEO_COMPARE_TEST").ok()) {
            static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if c.fields.len() >= 2 && ui.input(|i| i.time) > 9.0 && !DONE.swap(true, Ordering::Relaxed) {
                for part in spec.split(';') {
                    match part.split_once('=') {
                        Some(("ref", v)) => {
                            c.reference = c.fields.first().copied();
                            c.focus = c.fields.get(1).copied();
                            c.ref_view = match v {
                                "blink" => RefView::Blink,
                                "wipe" => RefView::Wipe,
                                "diff" => RefView::Difference,
                                _ => RefView::SideBySide,
                            };
                        }
                        Some(("check", v)) => {
                            for k in v.split(',') {
                                match k {
                                    "peaking" => c.checks.peaking = true,
                                    "clipping" => c.checks.clipping = true,
                                    "focus" => c.checks.focus_point = true,
                                    "exif" => c.checks.exif = true,
                                    _ => {}
                                }
                            }
                        }
                        Some(("tour", v)) => {
                            start_tournament(&mut c);
                            for d in v.chars() {
                                if let Some(t) = c.tournament.as_mut() {
                                    t.decide(if d == 'R' { Decision::Right } else { Decision::Left });
                                }
                            }
                        }
                        Some(("measure", v)) => {
                            let n: Vec<f32> = v.split(':').filter_map(|x| x.parse().ok()).collect();
                            if let [a, b, x, y] = n[..] {
                                c.checks.measure = true;
                                c.measure_rect = Some(Rect::from_min_max(egui::pos2(a, b), egui::pos2(x, y)));
                                self.compare_measure_values(&c);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        if let Some(m) = self.cmp.test_mode.take() {
            match m.as_str() {
                "light" | "all" => c.arrange = Arrange::All,
                "duel" | "tournament" => start_tournament(&mut c),
                "zoom" => self.cmp.test_mode = Some("zoom-now".into()),
                "zoom-now" => {}
                _ => {}
            }
        }
        let now = ui.input(|i| i.time);
        // ── rows above the fields
        // the ranking says what it is in the head row (the fields below stand in the order of the places)
        let head = match c.tournament.as_ref().filter(|_| c.ranking_shown()) {
            Some(t) => (crate::trf!("Ergebnis des Turniers · {a} · {b}", a = crate::dialog::count(t.entrants.len(), "Foto", "Fotos"), b = crate::dialog::count(t.duels(), "Duell", "Duelle")), Some(adopt_tip())),
            None => head,
        };
        self.compare_head(ui, &mut c, head);
        let avail = ui.available_rect_before_wrap();
        let persons = if c.faces_bar { self.lib.compare_persons(&c.images).len().min(3) } else { 0 };
        let faces_h = if persons > 0 { persons as f32 * 64.0 + 22.0 } else { 0.0 };
        let fields_area = Rect::from_min_max(avail.min, egui::pos2(avail.max.x, (avail.max.y - faces_h).max(avail.min.y + 120.0)));
        self.cmp.all_cap = pc::all_capacity(fields_area.size());
        ui.allocate_rect(avail, egui::Sense::hover());
        let mut large: Vec<i64> = Vec::new();
        large.extend(c.fields.iter().copied());
        self.compare_fields(ui, &mut c, fields_area, library, now);
        if persons > 0 {
            large.extend(c.images.iter().copied());
            let faces_area = Rect::from_min_max(egui::pos2(avail.min.x, fields_area.max.y + 4.0), avail.max);
            self.compare_faces_bar(ui, &c, faces_area);
        }
        large.dedup();
        self.lib.compare_request_previews(&large);
        // the arrangement chosen is remembered
        // (not the ranking's "Alle": the arrangement of before comes back after it)
        if !c.ranking_shown() {
            if c.arrange != self.cmp.arrange_saved.unwrap_or(c.arrange) {
                library::settings::set("compareArrange", serde_json::json!(c.arrange.key()));
            }
            self.cmp.arrange_saved = Some(c.arrange);
        }
        if c.fit != self.cmp.fit_saved.unwrap_or(c.fit) {
            library::settings::set("compareFit", serde_json::json!(c.fit.key()));
        }
        self.cmp.fit_saved = Some(c.fit);
        self.lib.compare = Some(c);
        if self.cmp.test_mode.as_deref() == Some("zoom-now") && self.lib.compare.as_ref().is_some_and(|c| c.fields.iter().all(|f| self.lib.compare_hint(*f).is_some())) {
            self.cmp.test_mode = None;
            self.compare_zoom_key(1.0);
        }
        self.compare_commands(&ui.ctx().clone());
    }

    /// The one slim head row: what is compared (time span and reason in the tooltip), on the right the way back.
    fn compare_head(&mut self, ui: &mut egui::Ui, c: &mut Compare, info: (String, Option<String>)) {
        let (head, tip) = info;
        let width = ui.available_width();
        ui.allocate_ui_with_layout(egui::vec2(width, 24.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.set_min_size(egui::vec2(width, 24.0));
            // the way back: a secondary button (framed grey; the bare words were not seen as a button)
            let back = lr::secondary_button(ui, Some(lr::Icon::X), "Vergleich schließen");
            lr::tip_key(ui, &back, "Zurück zur vorigen Ansicht", "cmp_back");
            if back.clicked() {
                c.pending.push(Cmd::Close);
            }
            // more photos than fields: which ones the fields show, and paging to the others (a series or any
            // selection); the strip shows all of them, a click there swaps one into the focused field
            if let Some(places) = c.page_text() {
                let n = c.images.len();
                // greyed without hover at the first / last page (they would do nothing)
                let arrow = |ui: &mut egui::Ui, text: &str, tip: &str, on: bool| {
                    let (r, resp) = ui.allocate_exact_size(Vec2::new(22.0, 22.0), if on { egui::Sense::click() } else { egui::Sense::hover() });
                    if on && resp.hovered() {
                        ui.painter().rect_filled(r, 0.0, Color32::from_gray(0x2a));
                    }
                    ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(13.0), if on { lr::TEXT() } else { Color32::from_gray(0x50) });
                    if on {
                        lr::hover_tip(ui, &resp, crate::tr(tip));
                    }
                    on && resp.clicked()
                };
                let (back, next) = (c.can_page(-1), c.can_page(1));
                if arrow(ui, "›", "Nächste Fotos in die Felder", next) {
                    c.page(1);
                }
                let label = crate::trf!("Felder {a} von {n}", a = places, n = n);
                let l = ui.add(egui::Label::new(egui::RichText::new(label).font(egui::FontId::proportional(12.0)).color(lr::TEXT())).sense(egui::Sense::hover()));
                lr::hover_tip(ui, &l, crate::tr("Mehr Fotos als Felder: die Pfeile blättern, ein Klick im Filmstreifen holt ein Foto ins gewählte Feld, die Anordnung „Alle“ zeigt bis zu 8"));
                if arrow(ui, "‹", "Vorige Fotos in die Felder", back) {
                    c.page(-1);
                }
                ui.add_space(8.0);
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                let h = ui.add(egui::Label::new(egui::RichText::new(head).font(lr::semibold(12.0)).color(Color32::from_gray(0xe0))).truncate().sense(egui::Sense::hover()));
                if let Some(t) = tip {
                    lr::hover_tip(ui, &h, &t);
                }
            });
        });
        ui.add_space(4.0);
    }

    /// The comparison's tools in the bar below the image (Develop) or below the fields (library), as symbols with
    /// tooltips and shortcuts, in groups; groups that do not fit go into "MEHR" (lowest rank first), nothing overlaps.
    pub(crate) fn compare_toolbar(&mut self, ui: &mut egui::Ui) {
        let Some(mut c) = self.lib.compare.take() else { return };
        let series = c.stack.is_some();
        let tour = c.mode == Mode::Tournament && c.tournament.as_ref().is_some_and(|t| !t.done);
        let ranking = c.ranking_shown();
        // groups in the rank they give way (first = first into "MEHR"); widths: 30 per symbol
        let mut groups: Vec<(Tool, f32)> = Vec::new();
        groups.push((Tool::Check, 180.0));
        groups.push((Tool::Reference, 60.0));
        groups.push((Tool::View, 120.0));
        groups.push((Tool::Decide, if tour { 180.0 } else { 60.0 }));
        groups.push((Tool::Mode, 60.0));
        let zoom_w = self.toolbar_widths.get("cmp-zoom").copied().unwrap_or(240.0);
        let main_w = if series || ranking { 110.0 } else { 0.0 };
        let gap = crate::toolbar_plan::GAP;
        let width = ui.available_width() - 20.0;
        let total = |g: &[(Tool, f32)], more: bool| zoom_w + main_w + g.iter().map(|x| x.1 + gap).sum::<f32>() + if more { crate::toolbar_plan::MORE_W + gap } else { 0.0 };
        let mut hidden: Vec<Tool> = Vec::new();
        while !groups.is_empty() && total(&groups, !hidden.is_empty() || total(&groups, false) > width) > width {
            hidden.push(groups.remove(0).0);
        }
        let shown = |t: Tool| groups.iter().any(|g| g.0 == t);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            if shown(Tool::Mode) {
                self.tool_mode(ui, &mut c);
            }
            if shown(Tool::View) {
                self.tool_view(ui, &mut c);
            }
            if shown(Tool::Check) {
                self.tool_check(ui, &mut c);
            }
            if shown(Tool::Reference) {
                self.tool_reference(ui, &mut c);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = gap;
                if ranking {
                    // the ranking's one main button (it takes the place of "Weiter" until it is taken over or dropped)
                    let adopt = lr::main_button(ui, crate::tr("Übernehmen"), true);
                    lr::hover_tip(ui, &adopt, &adopt_tip());
                    if adopt.clicked() {
                        c.pending.push(Cmd::Adopt);
                    }
                } else if series {
                    let next = lr::main_button(ui, crate::tr("Weiter"), true);
                    lr::tip_key(ui, &next, "Gesichtet, nächste Serie", "cmp_finish");
                    if next.clicked() {
                        c.pending.push(Cmd::Finish);
                    }
                }
                let z = ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    self.zoom_menu_in(ui, true);
                });
                self.toolbar_widths.insert("cmp-zoom", z.response.rect.width());
                if shown(Tool::Decide) {
                    self.tool_decide(ui, &mut c, tour);
                }
                if !hidden.is_empty() {
                    self.tool_more(ui, &mut c, &hidden, tour);
                }
            });
        });
        self.lib.compare = Some(c);
        self.compare_commands(&ui.ctx().clone());
    }

    fn tool_mode(&mut self, ui: &mut egui::Ui, c: &mut Compare) {
        let (t1, t2) = (lr::with_shortcut("Vergleich: Fotos in Feldern nebeneinander", "cmp_mode_compare"), lr::with_shortcut("Turnier: Duelle bis Platz 1–3", "cmp_mode_tournament"));
        let cells = [(lr::Cell::Symbol(lr::Icon::Images, lr::TEXT()), c.mode == Mode::Compare, t1.as_str()), (lr::Cell::Symbol(lr::Icon::Flag, lr::TEXT()), c.mode == Mode::Tournament, t2.as_str())];
        match lr::cell_group(ui, "cmp-modus", &cells, 30.0) {
            Some(0) => c.mode = Mode::Compare,
            Some(_) => start_tournament(c),
            None => {}
        }
    }

    /// Arrangement (one symbol with a menu), coupled, align.
    fn tool_view(&mut self, ui: &mut egui::Ui, c: &mut Compare) {
        let t_arr = lr::with_shortcut("Anordnung: Auto · 1 Reihe · 1 Spalte · 2 × 2 · Gewähltes groß · Alle", "cmp_arrange");
        let t_cpl = lr::with_shortcut(if c.coupled { "Gekoppelt: Zoom und Verschieben in allen Feldern gleich (einzeln: ⇧-Ziehen)" } else { "Entkoppelt: jedes Feld wird einzeln verschoben" }, "cmp_coupled");
        let t_aln = lr::with_shortcut("Ausrichten: Einzel-Versätze zurücksetzen", "cmp_align");
        let t_fit = lr::with_shortcut(if c.fit.fill() { "Füllen: jedes Foto füllt sein Feld (Ränder beschnitten) – Klick: ganz zeigen" } else { "Ganz zeigen: jedes Foto ganz im Feld – Klick: füllen" }, "cmp_fit");
        let arr_icon = match c.arrange {
            Arrange::Auto => lr::Icon::Auto,
            Arrange::Row => lr::Icon::SplitH,
            Arrange::Column => lr::Icon::ArrowsV,
            Arrange::Square => lr::Icon::SquaresFour,
            Arrange::FocusLarge => lr::Icon::ImageSquare,
            Arrange::All => lr::Icon::GridNine,
        };
        let cells = [
            (lr::Cell::Symbol(arr_icon, lr::TEXT()), false, t_arr.as_str()),
            (lr::Cell::Symbol(if c.coupled { lr::Icon::Lock } else { lr::Icon::LockOpen }, lr::TEXT()), c.coupled, t_cpl.as_str()),
            (lr::Cell::Symbol(lr::Icon::Target, lr::TEXT()), false, t_aln.as_str()),
            (lr::Cell::Symbol(if c.fit.fill() { lr::Icon::FrameCorners } else { lr::Icon::CornersOut }, lr::TEXT()), false, t_fit.as_str()),
        ];
        // every cell acts visibly or is greyed with the reason (found in 1.5.0-test.6)
        let state = view_states(c, &self.cmp.same_layout);
        let tips: Vec<String> = cells.iter().zip(&state).map(|((_, _, t), (_, why))| why.map_or_else(|| t.to_string(), |w| crate::tr(w).to_string())).collect();
        let cells: Vec<(lr::Cell, bool, &str)> = cells.iter().zip(&tips).map(|((cell, on, _), t)| (*cell, *on, t.as_str())).collect();
        let enabled: Vec<bool> = state.iter().map(|x| x.0).collect();
        let (click, rect) = lr::cell_group_enabled(ui, "cmp-ansicht", &cells, &enabled, 30.0, lr::BAR_H);
        let id = egui::Id::unique("cmp-anordnung-menu");
        match click {
            Some(0) => egui::Popup::toggle_id(ui.ctx(), id),
            Some(1) => c.coupled = !c.coupled,
            Some(2) => c.offsets.clear(),
            Some(_) => c.fit = c.fit.toggled(),
            None => {}
        }
        let anchor = Rect::from_min_size(rect.min, Vec2::new(30.0, rect.height()));
        let same = self.cmp.same_layout.clone();
        popup_above(ui, id, anchor, |ui| {
            for (a, t) in ARRANGEMENTS {
                // an arrangement that gives the very fields shown now is greyed (e.g. "2 × 2" with two photos)
                let dup = same.contains(&a);
                let name = arrange_name(a, t, c.images.len());
                let text = if dup { format!("{name} – {}", crate::tr("wie jetzt")) } else { name };
                if lr::menu_item(ui, &check_text(c.arrange == a, &text), "", !dup) {
                    c.arrange = a;
                }
            }
        });
    }

    /// Inspection aids, each a switch acting in all fields.
    fn tool_check(&mut self, ui: &mut egui::Ui, c: &mut Compare) {
        let tips = [
            lr::with_shortcut("Fokuspunkt der Kamera", "cmp_focus_point"),
            lr::with_shortcut("Kanten zeigen: gleiche Schwelle in allen Feldern", "cmp_peaking"),
            lr::with_shortcut("Clipping", "cmp_clipping"),
            lr::with_shortcut("Aufnahme: Zeit, Blende, ISO, Brennweite; Abweichungen hervorgehoben", "cmp_exif"),
            lr::with_shortcut("Messen: Rechteck ziehen, Schärfe je Feld", "cmp_measure"),
            lr::with_shortcut("Gesichterleiste", "cmp_faces"),
        ];
        let k = &c.checks;
        let cells = [
            (lr::Cell::Symbol(lr::Icon::Crosshair, lr::TEXT()), k.focus_point, tips[0].as_str()),
            (lr::Cell::Symbol(lr::Icon::Waveform, lr::TEXT()), k.peaking, tips[1].as_str()),
            (lr::Cell::Symbol(lr::Icon::Warning, lr::TEXT()), k.clipping, tips[2].as_str()),
            (lr::Cell::Symbol(lr::Icon::Info, lr::TEXT()), k.exif, tips[3].as_str()),
            (lr::Cell::Symbol(lr::Icon::Ruler, lr::TEXT()), k.measure, tips[4].as_str()),
            (lr::Cell::Symbol(lr::Icon::Smiley, lr::TEXT()), c.faces_bar, tips[5].as_str()),
        ];
        let state = self.check_states(c);
        let tips2: Vec<String> = cells.iter().zip(&state).map(|((_, _, t), (_, why))| why.map_or_else(|| t.to_string(), |w| crate::tr(w).to_string())).collect();
        let cells: Vec<(lr::Cell, bool, &str)> = cells.iter().zip(&tips2).map(|((cell, on, _), t)| (*cell, *on, t.as_str())).collect();
        let enabled: Vec<bool> = state.iter().map(|x| x.0).collect();
        if let Some(i) = lr::cell_group_enabled(ui, "cmp-pruefen", &cells, &enabled, 30.0, lr::BAR_H).0 {
            toggle_check(c, i);
        }
    }

    /// Reference: hold the focused photo, and how the candidate is shown against it (one symbol with a menu).
    fn tool_reference(&mut self, ui: &mut egui::Ui, c: &mut Compare) {
        let held = c.reference.is_some();
        let t_hold = lr::with_shortcut(if held { "Referenz aufheben" } else { "Fokussiertes Foto als Referenz festhalten" }, "cmp_reference");
        let tour = c.mode == Mode::Tournament;
        let t_view = if tour {
            crate::tr("Im Turnier gibt es keine Referenz").to_string()
        } else if held {
            crate::tr("Darstellung gegen die Referenz: Nebeneinander, Blinken (W), Wischer (⇧W), Differenz (⌥W)").to_string()
        } else {
            crate::tr("Erst eine Referenz festhalten (R)").to_string()
        };
        let t_hold = if tour { crate::tr("Im Turnier gibt es keine Referenz").to_string() } else { t_hold };
        let view_icon = match c.ref_view {
            RefView::SideBySide => lr::Icon::SplitH,
            RefView::Blink => lr::Icon::Eye,
            RefView::Wipe => lr::Icon::ArrowsH,
            RefView::Difference => lr::Icon::CircleHalf,
        };
        let cells = [
            (lr::Cell::Symbol(lr::Icon::PushPin, if held { lr::GOLD } else { lr::TEXT() }), held, t_hold.as_str()),
            (lr::Cell::Symbol(view_icon, lr::TEXT()), held && c.ref_view != RefView::SideBySide, t_view.as_str()),
        ];
        let enabled = [!tour && (c.focus.is_some() || held), held && !tour];
        let (click, rect) = lr::cell_group_enabled(ui, "cmp-referenz", &cells, &enabled, 30.0, lr::BAR_H);
        let id = egui::Id::unique("cmp-referenz-menu");
        match click {
            Some(0) => {
                if let Some(f) = c.focus.or(c.reference) {
                    c.toggle_reference(f);
                }
            }
            Some(_) if held => egui::Popup::toggle_id(ui.ctx(), id),
            _ => {}
        }
        let anchor = Rect::from_min_size(rect.min + Vec2::new(30.0, 0.0), Vec2::new(30.0, rect.height()));
        popup_above(ui, id, anchor, |ui| {
            for (v, t, k) in [(RefView::SideBySide, "Nebeneinander", ""), (RefView::Blink, "Blinken", "cmp_blink"), (RefView::Wipe, "Wischer", "cmp_wipe"), (RefView::Difference, "Differenz (Prüfhilfe)", "cmp_difference")] {
                if lr::menu_item(ui, &check_text(c.ref_view == v, crate::tr(t)), &crate::shortcuts::display(k), true) {
                    c.ref_view = v;
                    self.cmp.blink_start = ui.input(|i| i.time);
                }
            }
        });
    }

    /// Decisions: best photo and rest, or in the tournament keep left / right, equal, both out, last duel.
    fn tool_decide(&mut self, ui: &mut egui::Ui, c: &mut Compare, tour: bool) {
        if c.ranking_shown() {
            // the ranking: last duel back (⌘Z) or drop it (Esc); "Übernehmen" is the main button
            let tips = [lr::with_shortcut("Letztes Duell zurücknehmen", "cmp_last_duel"), lr::with_shortcut("Ergebnis verwerfen: nichts wird gesetzt", "cmp_back")];
            let cells = [(lr::Cell::Symbol(lr::Icon::RotCcw, lr::TEXT()), false, tips[0].as_str()), (lr::Cell::Symbol(lr::Icon::X, lr::TEXT()), false, tips[1].as_str())];
            match lr::cell_group(ui, "cmp-rangliste", &cells, 30.0) {
                Some(0) => {
                    c.undo_duel();
                }
                Some(_) => discard_ranking(c),
                None => {}
            }
            return;
        }
        if tour {
            let tips = [crate::tr("Linkes Foto behalten (←)").to_string(), crate::tr("Rechtes Foto behalten (→)").to_string(), lr::with_shortcut("Gleich gut: beide bleiben", "cmp_tie"), lr::with_shortcut("Beide raus", "cmp_both_out"), lr::with_shortcut("Letztes Duell zurücknehmen", "cmp_last_duel"), crate::tr("Turnier abbrechen: nichts wird gesetzt (anhalten: Esc)").to_string()];
            let cells = [
                (lr::Cell::Text("←", 16.0, lr::TEXT()), false, tips[0].as_str()),
                (lr::Cell::Text("→", 16.0, lr::TEXT()), false, tips[1].as_str()),
                (lr::Cell::Text("=", 15.0, lr::TEXT()), false, tips[2].as_str()),
                (lr::Cell::Symbol(lr::Icon::XSquare, lr::TEXT()), false, tips[3].as_str()),
                (lr::Cell::Symbol(lr::Icon::RotCcw, lr::TEXT()), false, tips[4].as_str()),
                (lr::Cell::Symbol(lr::Icon::X, lr::TEXT()), false, tips[5].as_str()),
            ];
            // right to left: the group is placed as one
            match lr::cell_group(ui, "cmp-duell", &cells, 30.0) {
                Some(5) => {
                    c.tournament = None;
                    c.mode = Mode::Compare;
                }
                Some(4) => {
                    if let Some(t) = c.tournament.as_mut() {
                        t.undo();
                    }
                }
                Some(i) => {
                    if let Some(t) = c.tournament.as_mut() {
                        t.decide([Decision::Left, Decision::Right, Decision::Tie, Decision::BothOut][i]);
                    }
                }
                None => {}
            }
            return;
        }
        let (t_best, t_rest) = (lr::with_shortcut("Fokussiertes Foto als bestes: Markierung „Auswahl“ und Titelfoto", "cmp_choose"), lr::with_shortcut("Rest ablehnen: alle Fotos außer den markierten (ein ⌘Z)", "cmp_rest"));
        let t_best = if c.focus.is_some() { t_best } else { crate::tr("Erst ein Foto fokussieren (Klick)").to_string() };
        let cells = [(lr::Cell::Symbol(lr::Icon::CheckCircle, lr::TEXT()), false, t_best.as_str()), (lr::Cell::Symbol(lr::Icon::XCircle, lr::TEXT()), false, t_rest.as_str())];
        match lr::cell_group_enabled(ui, "cmp-entscheiden", &cells, &[c.focus.is_some(), true], 30.0, lr::BAR_H).0 {
            Some(0) => {
                if let Some(f) = c.focus {
                    c.pending.push(Cmd::Best(f));
                }
            }
            Some(_) => c.pending.push(Cmd::Rest),
            None => {}
        }
    }

    /// "MEHR": the groups that did not fit, one function per row.
    fn tool_more(&mut self, ui: &mut egui::Ui, c: &mut Compare, hidden: &[Tool], tour: bool) {
        let resp = ui.add(egui::Button::new(crate::tr("MEHR")).min_size(egui::vec2(crate::toolbar_plan::MORE_W, lr::BAR_H)).fill(lr::GROUP_FILL));
        egui::Popup::menu(&resp).align(egui::RectAlign::TOP_START).show(|ui| {
            ui.set_min_width(260.0);
            let key = |k: &str| crate::shortcuts::display(k);
            for g in hidden {
                match g {
                    Tool::Check => {
                        let names = [("Fokuspunkt", "cmp_focus_point"), ("Kanten zeigen", "cmp_peaking"), ("Clipping", "cmp_clipping"), ("Aufnahme", "cmp_exif"), ("Messen", "cmp_measure"), ("Gesichterleiste", "cmp_faces")];
                        let on = [c.checks.focus_point, c.checks.peaking, c.checks.clipping, c.checks.exif, c.checks.measure, c.faces_bar];
                        let state = self.check_states(c);
                        for (i, (t, k)) in names.iter().enumerate() {
                            if lr::menu_item(ui, &check_text(on[i] && state[i].0, crate::tr(t)), &key(k), state[i].0) {
                                toggle_check(c, i);
                            }
                        }
                    }
                    Tool::Reference => {
                        let hold = if c.reference.is_some() { crate::tr("Referenz aufheben") } else { crate::tr("Fokussiertes Foto als Referenz festhalten") };
                        if lr::menu_item(ui, hold, &key("cmp_reference"), c.focus.is_some() && c.mode != Mode::Tournament) {
                            if let Some(f) = c.focus.or(c.reference) {
                                c.toggle_reference(f);
                            }
                        }
                        for (v, t, k) in [(RefView::SideBySide, "Nebeneinander", ""), (RefView::Blink, "Blinken", "cmp_blink"), (RefView::Wipe, "Wischer", "cmp_wipe"), (RefView::Difference, "Differenz (Prüfhilfe)", "cmp_difference")] {
                            if lr::menu_item(ui, &check_text(c.ref_view == v, crate::tr(t)), &key(k), c.reference.is_some() && c.mode == Mode::Compare) {
                                c.ref_view = v;
                            }
                        }
                    }
                    Tool::View => {
                        let state = view_states(c, &self.cmp.same_layout);
                        for (a, t) in ARRANGEMENTS {
                            if lr::menu_item(ui, &check_text(c.arrange == a, &arrange_name(a, t, c.images.len())), &key("cmp_arrange"), state[0].0 && !self.cmp.same_layout.contains(&a)) {
                                c.arrange = a;
                            }
                        }
                        if lr::menu_item(ui, &check_text(c.coupled, crate::tr("Gekoppelt")), &key("cmp_coupled"), state[1].0) {
                            c.coupled = !c.coupled;
                        }
                        if lr::menu_item(ui, crate::tr("Ausrichten"), &key("cmp_align"), state[2].0) {
                            c.offsets.clear();
                        }
                        for (f, t) in [(Fit::Fill, "Füllen"), (Fit::Whole, "Ganz zeigen")] {
                            if lr::menu_item(ui, &check_text(c.fit == f, crate::tr(t)), &key("cmp_fit"), state[3].0) {
                                c.fit = f;
                            }
                        }
                    }
                    Tool::Decide if tour => {
                        for (d, t, k) in [(Decision::Left, "Linkes Foto behalten", ""), (Decision::Right, "Rechtes Foto behalten", ""), (Decision::Tie, "Gleich gut", "cmp_tie"), (Decision::BothOut, "Beide raus", "cmp_both_out")] {
                            if lr::menu_item(ui, crate::tr(t), &key(k), true) {
                                if let Some(t) = c.tournament.as_mut() {
                                    t.decide(d);
                                }
                            }
                        }
                        // the same functions as the duel group in the bar (in a narrow window only here)
                        if lr::menu_item(ui, crate::tr("Letztes Duell zurücknehmen"), &key("cmp_last_duel"), true) {
                            if let Some(t) = c.tournament.as_mut() {
                                t.undo();
                            }
                        }
                        if lr::menu_item(ui, crate::tr("Turnier abbrechen"), "", true) {
                            c.tournament = None;
                            c.mode = Mode::Compare;
                        }
                    }
                    Tool::Decide if c.ranking_shown() => {
                        if lr::menu_item(ui, crate::tr("Letztes Duell zurücknehmen"), &key("cmp_last_duel"), true) {
                            c.undo_duel();
                        }
                        if lr::menu_item(ui, crate::tr("Ergebnis verwerfen"), &key("cmp_back"), true) {
                            discard_ranking(c);
                        }
                    }
                    Tool::Decide => {
                        if lr::menu_item(ui, crate::tr("Als bestes"), &key("cmp_choose"), c.focus.is_some()) {
                            if let Some(f) = c.focus {
                                c.pending.push(Cmd::Best(f));
                            }
                        }
                        if lr::menu_item(ui, crate::tr("Rest ablehnen"), &key("cmp_rest"), true) {
                            c.pending.push(Cmd::Rest);
                        }
                    }
                    Tool::Mode => {
                        for (m, t, k) in [(Mode::Compare, "Vergleich", "cmp_mode_compare"), (Mode::Tournament, "Turnier", "cmp_mode_tournament")] {
                            if lr::menu_item(ui, &check_text(c.mode == m, crate::tr(t)), &key(k), true) {
                                if m == Mode::Tournament {
                                    start_tournament(c);
                                } else {
                                    c.mode = m;
                                }
                            }
                        }
                    }
                }
            }
        });
    }

    // ───────────────────────────────────────────── Fields

    /// What a field shows: the photo on screen in Develop (live), otherwise the library's large preview.
    fn compare_field_pic(&mut self, image: i64) -> (Option<Pic>, Option<(Frame, [usize; 2])>, f32, f32) {
        let index = self.entry_of(image);
        let row = self.lib.compare_row(image);
        // the opened photo (Develop's or the cache's): frame and size
        let loaded = index.and_then(|i| {
            if let Some(c) = self.current.as_ref().filter(|c| c.index == i && self.loading.is_none()) {
                return Some((c.loaded.full_size, c.loaded.cal.orientation));
            }
            self.cache.iter().find(|(k, _)| *k == i).map(|(_, l)| (l.full_size, l.cal.orientation))
        });
        let frame = match (index, loaded) {
            (Some(i), Some(([fw, fh], o))) if fw > 0 && fh > 0 => Some((Frame::from_settings(&self.compare_settings(i), fw, fh, o), [fw, fh])),
            _ => None,
        };
        // the development of the field with the settings of now
        let dev = index.and_then(|i| self.cmp.developed.get(&image).filter(|d| d.0 == self.entries[i].settings)).map(|d| (d.1.clone(), d.2.clone(), d.3));
        let pixels = frame.as_ref().map(|(f, _)| f.w).or(dev.as_ref().map(|d| d.1.w)).or_else(|| row.as_ref().and_then(|r| Some(r.width?.max(r.height?) as f32))).unwrap_or(0.0);
        // Develop: the photo being edited shows its live development
        if !self.in_grid() {
            if let (Some(c), Some((f, [fw, fh]))) = (self.current.as_ref().filter(|c| Some(c.index) == index && self.loading.is_none()), frame.as_ref()) {
                if let Some(t) = &c.texture {
                    let w = t.size_vec2().x;
                    return (Some(Pic::Frame(t.clone(), f.clone(), [0.0, 0.0, *fw as f32, *fh as f32])), frame.clone(), w * f.w / *fw as f32, pixels);
                }
            }
        }
        // without the opened photo the development's frame serves (the same settings, the same geometry)
        let frame = frame.or_else(|| dev.as_ref().map(|d| (d.1.clone(), d.2)));
        if let Some((t, f, [fw, fh])) = dev {
            let w = t.size_vec2().x;
            let preview_px = w * f.w / fw.max(1) as f32;
            return (Some(Pic::Frame(t, f, [0.0, 0.0, fw as f32, fh as f32])), frame, preview_px, pixels);
        }
        match self.lib.compare_texture(image) {
            Some(t) => {
                let w = t.size_vec2().x;
                (Some(Pic::Plain(t)), frame, w, pixels)
            }
            None => (None, frame, 0.0, pixels),
        }
    }

    /// The settings a field is developed with: the photo's own, or – a RAW without own settings – the Lightroom start
    /// values Develop would open it with (known once the photo was opened).
    fn compare_settings(&self, index: usize) -> Settings {
        let e = &self.entries[index];
        match self.cmp.start.get(&e.image) {
            Some(s) if !e.stored && e.settings == Settings::default() => Settings { view: e.settings.view, ..s.clone() },
            _ => e.settings.clone(),
        }
    }

    /// [`Self::compare_settings`] for the results of the jobs (`main.rs`).
    pub(crate) fn compare_settings_of(&self, index: usize) -> Settings {
        self.compare_settings(index)
    }

    /// The opened photo of an entry (Develop's or the cache's).
    fn compare_loaded(&self, index: usize) -> Option<&crate::jobs::Loaded> {
        if let Some(c) = self.current.as_ref().filter(|c| c.index == index && self.loading.is_none()) {
            return Some(&c.loaded);
        }
        self.cache.iter().find(|(k, _)| *k == index).map(|(_, l)| l)
    }

    /// Develop every visible field with its settings, the focused one first, one job at a time. Returns the photos
    /// that must be opened for it (in that order).
    fn compare_develop_fields(&mut self, c: &Compare, visible: &[i64], now: f64) -> Vec<i64> {
        let library = self.in_grid();
        let mut order: Vec<i64> = c.focus.filter(|f| visible.contains(f)).into_iter().collect();
        order.extend(visible.iter().copied().filter(|v| Some(*v) != c.focus));
        // a job that never came back (photo dropped from the cache meanwhile): ask again
        if self.cmp.develop_pending.as_ref().is_some_and(|(_, _, t)| now - t > 30.0) {
            self.cmp.develop_pending = None;
        }
        let mut need = Vec::new();
        // a photo not in the list (yet): nothing to develop from here, and no "all full" for the log
        let mut unknown = false;
        self.cmp.states.retain(|k, _| visible.contains(k));
        self.cmp.developed.retain(|k, _| visible.contains(k));
        for img in order {
            let Some(i) = self.entry_of(img) else {
                self.cmp.states.insert(img, FieldState::Full);
                unknown = true;
                continue;
            };
            let e = &self.entries[i];
            // Develop: the photo on screen is live
            if !library && self.current.as_ref().is_some_and(|x| x.index == i && x.texture.is_some()) && self.loading.is_none() {
                // (after Develop gave the exact base up it shows the fast preview: the field says so)
                let fast = self.refine_retries >= 2 && self.current.as_ref().is_some_and(|x| crate::preview_hold::waits_for_base(&x.loaded, &e.settings));
                self.cmp.states.insert(img, if fast { FieldState::Fast } else { FieldState::Full });
                continue;
            }
            // nothing to develop (video, a format without development): the library preview is the photo
            if !crate::image_kind::of(&e.path).editable() {
                self.cmp.states.insert(img, FieldState::Full);
                continue;
            }
            if self.cmp.developed.get(&img).is_some_and(|d| d.0 == e.settings) {
                // developed without the exact base (it failed): the field says so
                let fast = self.compare_base_given_up(img, i) && self.compare_loaded(i).is_some_and(|l| crate::preview_hold::waits_for_base(l, &self.compare_settings(i)));
                self.cmp.states.insert(img, if fast { FieldState::Fast } else { FieldState::Full });
                continue;
            }
            if !crate::developable_state(e.state) {
                self.cmp.states.insert(img, FieldState::Offline);
                continue;
            }
            let state = if self.cmp.pool_refused && self.compare_loaded(i).is_none() { FieldState::Memory } else { FieldState::Computing };
            self.cmp.states.insert(img, state);
            let Some(l) = self.compare_loaded(i) else {
                need.push(img);
                continue;
            };
            // the start values Develop would use for a RAW without own settings
            if l.previews.foreign.is_some() && !e.stored && e.settings == Settings::default() && !self.cmp.start.contains_key(&img) {
                if let Some(s) = crate::resolve_lr_start(l, e.copy.as_deref()) {
                    self.cmp.start.insert(img, s);
                }
            }
            let settings = self.compare_settings(i);
            let entry_settings = self.entries[i].settings.clone();
            let l = self.compare_loaded(i).unwrap();
            let ([fw, fh], o) = (l.full_size, l.cal.orientation);
            if fw == 0 || fh == 0 {
                continue;
            }
            // developed while preloading with these settings: shown at once
            if let Some((_, w, h, rgb)) = l.prerendered.as_ref().filter(|p| p.0 == settings) {
                crate::log(&format!("compare: developed {} {w}×{h} while opening", e.name));
                let tex = self.worker.ctx().load_texture(format!("cmp-dev-{img}"), egui::ColorImage::from_rgb([*w, *h], rgb), egui::TextureOptions::LINEAR);
                self.cmp.developed.insert(img, (entry_settings, tex, Frame::from_settings(&settings, fw, fh, o), [fw, fh]));
                self.cmp.states.insert(img, FieldState::Full);
                continue;
            }
            // a photo whose fast preview is too rough to be shown (high ISO): its field is developed from the exact
            // base only, as in Develop – the library preview stays ("wird berechnet") until the base is there
            if !self.compare_base_given_up(img, i) && crate::preview_hold::waits_for_base(l, &settings) {
                self.compare_request_base(img, i, now);
                continue;
            }
            if self.cmp.develop_pending.is_some() {
                continue;
            }
            let job = crate::jobs::Job::HoverRender { index: i, previews: l.previews.clone(), cal: l.cal.clone(), settings, supersede: false };
            self.cmp.develop_pending = Some((img, entry_settings, now));
            self.worker.send(job);
        }
        // the time from showing these fields until every one is in full quality (measurements, log)
        if !self.cmp.full_logged && !unknown && !visible.is_empty() && visible.iter().all(|b| matches!(self.cmp.states.get(b), Some(FieldState::Full | FieldState::Fast))) {
            self.cmp.full_logged = true;
            crate::log(&format!("compare: all {} fields in full quality after {:.1} s", visible.len(), now - self.cmp.shown_at));
        }
        need
    }

    /// Ask for the exact base of the opened photo of a field, one at a time (the result lands in the opened photo:
    /// `JobResult::SppReady` with `prefetch`; the field is developed in a later frame once the base is there).
    fn compare_request_base(&mut self, image: i64, index: usize, now: f64) {
        // a base that never came back: ask again
        if self.cmp.base_pending.as_ref().is_some_and(|(_, _, t)| now - t > 180.0) {
            self.cmp.base_pending = None;
        }
        if self.cmp.base_pending.is_some() {
            return;
        }
        // Develop computes the base of the photo on screen itself (and one ahead)
        if (self.current.as_ref().is_some_and(|c| c.index == index) && !self.in_grid()) || self.spp_prefetching.as_ref().is_some_and(|(i, _)| *i == index) {
            return;
        }
        let Some(session) = self.compare_loaded(index).and_then(|l| l.session.clone()) else { return };
        let e = &self.entries[index];
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.cmp.base_pending = Some((image, cancel.clone(), now));
        crate::log(&format!("compare: exact base for {} (fast preview withheld)", e.name));
        self.worker.send(crate::jobs::Job::Spp { index, path: e.path.clone(), session: Some(session), mode: e.settings.spp_key(&e.path), cancel, prefetch: true });
    }

    /// The exact base of a photo arrived or failed (`JobResult::SppReady` / `SppFailed`, asked for by the comparison
    /// or by Develop for the photo on screen): a failed one is remembered, its field falls back to the fast preview.
    pub(crate) fn compare_base_done(&mut self, index: usize, failed: bool) {
        let Some(image) = self.entries.get(index).map(|e| e.image) else { return };
        if self.cmp.base_pending.as_ref().is_some_and(|p| p.0 == image) {
            self.cmp.base_pending = None;
        }
        if failed && self.cmp.shown_last.contains(&image) && self.cmp.base_failed.insert(image) {
            crate::log(&format!("compare: exact base for {} failed, field from the fast preview", self.entries[index].name));
        }
    }

    /// The answer to the base the comparison asked for was lost (the list changed while it was computed, the result
    /// was discarded): ask again at once instead of waiting for the time limit.
    pub(crate) fn compare_base_lost(&mut self) {
        if self.cmp.base_pending.take().is_some() {
            crate::log("compare: answer for the exact base lost, asking again");
        }
    }

    /// Is the exact base of this field's photo not to be waited for: it failed, or Develop gave it up for the photo
    /// on screen (the fast preview is the best picture there is then)?
    fn compare_base_given_up(&self, image: i64, index: usize) -> bool {
        self.cmp.base_failed.contains(&image) || (self.refine_retries >= 2 && self.current.as_ref().is_some_and(|c| c.index == index))
    }

    /// A field's development arrived (`JobResult::HoverRendered`): the comparison's? (`true`: taken.)
    pub(crate) fn compare_take_developed(&mut self, ctx: &egui::Context, index: usize, settings: &Settings, width: usize, height: usize, rgb: &[u8]) -> bool {
        let Some(e) = self.entries.get(index) else { return false };
        let image = e.image;
        let ours = self.cmp.develop_pending.as_ref().is_some_and(|(b, s, _)| *b == image && *s == e.settings);
        if !ours || self.compare_settings(index) != *settings {
            return false;
        }
        let (_, entry_settings, sent) = self.cmp.develop_pending.take().unwrap();
        let Some(([fw, fh], o)) = self.compare_loaded(index).map(|l| (l.full_size, l.cal.orientation)) else { return true };
        let tex = ctx.load_texture(format!("cmp-dev-{image}"), egui::ColorImage::from_rgb([width, height], rgb), egui::TextureOptions::LINEAR);
        self.cmp.developed.insert(image, (entry_settings, tex, Frame::from_settings(settings, fw, fh, o), [fw, fh]));
        let now = ctx.input(|i| i.time);
        crate::log(&format!("compare: developed {} {width}×{height} in {:.0} ms ({} fields developed)", e.name, (now - sent) * 1000.0, self.cmp.developed.len()));
        ctx.request_repaint();
        true
    }

    /// A field's development was dropped (photo gone from the cache, superseded): ask again.
    pub(crate) fn compare_develop_dropped(&mut self, index: usize) {
        let image = self.entries.get(index).map(|e| e.image);
        if image.is_some() && self.cmp.develop_pending.as_ref().map(|p| p.0) == image {
            self.cmp.develop_pending = None;
        }
    }

    /// The fields shown changed (paging, another arrangement, a photo swapped): what was asked for photos no longer
    /// shown is cancelled – the crop on its way, the development, the photo being opened.
    fn compare_cancel_hidden(&mut self, visible: &[i64], now: f64) {
        if self.cmp.shown_last == visible {
            return;
        }
        self.cmp.shown_last = visible.to_vec();
        self.cmp.shown_at = now;
        self.cmp.full_logged = false;
        if self.cmp.pending.as_ref().is_some_and(|p| !visible.contains(&p.0)) {
            self.cmp.pending = None;
            self.cmp.generation.fetch_add(1, Ordering::SeqCst);
        }
        if self.cmp.develop_pending.as_ref().is_some_and(|p| !visible.contains(&p.0)) {
            self.cmp.develop_pending = None;
        }
        if let Some((_, cancel, _)) = self.cmp.base_pending.take_if(|p| !visible.contains(&p.0)) {
            cancel.store(true, Ordering::SeqCst);
        }
        self.cmp.base_failed.retain(|b| visible.contains(b));
        if let Some(i) = self.cmp.opening {
            if self.entries.get(i).is_none_or(|e| !visible.contains(&e.image)) {
                if let Some((_, flag)) = self.prefetch_flags.iter().find(|(n, _)| *n == i) {
                    flag.store(true, Ordering::SeqCst);
                    crate::log(&format!("compare: opening {} cancelled (no longer shown)", self.entries[i].name));
                }
                self.cmp.opening = None;
            }
        }
    }

    fn compare_fields(&mut self, ui: &mut egui::Ui, c: &mut Compare, area: Rect, library: bool, now: f64) {
        let tournament = c.mode == Mode::Tournament;
        let below = FOOT_H;
        let _ = library;
        // reference shown against the candidate in one large field
        let single_ref = c.reference.is_some() && c.ref_view != RefView::SideBySide && !tournament;
        let shown: Vec<i64> = if single_ref { c.reference.into_iter().chain(c.candidate()).collect() } else { c.fields.clone() };
        let n = if single_ref { 1 } else { shown.len().max(1) };
        if shown.is_empty() {
            ui.painter().text(area.center(), egui::Align2::CENTER_CENTER, crate::tr("Im Filmstreifen Fotos mit ⌘-Klick auswählen"), egui::FontId::proportional(13.0), lr::TEXT_DIM());
            return;
        }
        if tournament {
            self.compare_duel_head(ui, c, area);
        }
        let area = if tournament { Rect::from_min_max(area.min + Vec2::new(0.0, 34.0), area.max) } else { area };
        let mut infos: Vec<(i64, Option<Pic>, Option<(Frame, [usize; 2])>, f32, f32, f32)> = Vec::new();
        for &img in &shown {
            let (pic, frame, preview_px, pixels) = self.compare_field_pic(img);
            let aspect = match &pic {
                Some(Pic::Plain(t)) => t.size_vec2().x / t.size_vec2().y.max(1.0),
                Some(Pic::Frame(_, f, _)) => f.w / f.h.max(1.0),
                None => self.lib.compare_row(img).and_then(|r| Some(r.width? as f32 / r.height?.max(1) as f32)).unwrap_or(1.5),
            };
            // without the opened photo: its longer side from the library, the width by the aspect
            let pixels = if frame.is_some() || aspect >= 1.0 { pixels } else { pixels * aspect };
            infos.push((img, pic, frame, preview_px, pixels, aspect));
        }
        // the photos' shapes decide the arrangement ("whole": the median, "fill": the least crop); it depends on the
        // area only, never on the zoom
        let aspects: Vec<f32> = infos.iter().map(|i| i.5).collect();
        let arrange = if tournament { Arrange::Auto } else { c.arrange };
        let focus_k = c.focus.and_then(|f| infos.iter().position(|i| i.0 == f)).unwrap_or(0);
        let fill = c.fit.fill();
        let gap = if fill { FILL_GAP } else { GAP };
        let cells = if single_ref { vec![area] } else { pc::layout(n, arrange, area, &aspects, below, gap, focus_k, fill) };
        self.cmp.same_layout = if single_ref || tournament { Vec::new() } else { pc::same_layouts(n, arrange, area, &aspects, below, gap, focus_k, fill) };
        // more photos than four: "Alle" shows other fields than the others (more of them), whatever their grid
        if c.images.len() > pc::MAX_FIELDS {
            if arrange == Arrange::All {
                self.cmp.same_layout.clear();
            } else {
                self.cmp.same_layout.retain(|a| *a != Arrange::All);
            }
        }
        let mut fields: Vec<Field> = Vec::new();
        for (k, (img, pic, frame, preview_px, pixels, aspect)) in infos.into_iter().enumerate() {
            let cell = if single_ref { cells[0] } else { cells[k.min(cells.len() - 1)] };
            let space = Rect::from_min_max(cell.min, egui::pos2(cell.max.x, cell.max.y - below));
            // one card per photo: the photo (fitted) with its foot right below it, the card centred in its cell
            // filled: the photo covers its whole field (the cells cover the area)
            let rect = if single_ref || fill {
                space
            } else {
                let fit = pc::fit_size(space.size(), aspect).floor();
                let top = cell.top() + ((cell.height() - fit.y - FOOT_H) / 2.0).max(0.0);
                Rect::from_min_size(egui::pos2(space.center().x - fit.x / 2.0, top), fit)
            };
            fields.push(Field { image: img, rect, aspect, pic, frame, preview_px, pixels });
        }
        // ── zoom state: the focused (or first) field defines fit and 100 %
        let ppp = ui.ctx().pixels_per_point();
        let fk = c.focus.and_then(|f| fields.iter().position(|x| x.image == f)).unwrap_or(0);
        let fit0 = pc::base_size(fields[fk].rect.size(), fields[fk].aspect, fill);
        let one = if fields[fk].pixels > 0.0 { fields[fk].pixels / (fit0.x * ppp) } else { 1.0 };
        let shifts = self.compare_shifts(c, &fields);
        // offset of each field's centre against the shared one (alignment shift, a field moved on its own)
        let offset_of = |c: &Compare, k: usize| shifts[k] + c.offsets.get(&fields[k].image).copied().unwrap_or(Vec2::ZERO);
        // the toolbar's zoom group (Develop) changed the pan (it shows the focused field): take its centre
        if (self.pan - self.cmp.last_pan).length() > 0.5 {
            let size = fit0 * self.zoom.max(1.0);
            self.cmp.center = Vec2::splat(0.5) - self.pan / size.max(Vec2::splat(1.0)) - offset_of(c, fk);
        }
        self.one_to_one = one;
        self.fit_size = fit0;
        self.view_size = fields[fk].rect.size();
        // ── input over the fields
        let resp = ui.interact(area, egui::Id::unique("cmp-felder"), egui::Sense::click_and_drag());
        let pointer = resp.hover_pos().or(resp.interact_pointer_pos());
        let hovered = pointer.and_then(|p| fields.iter().position(|f| f.rect.contains(p)));
        let center_of = |s: &Self, c: &Compare, k: usize| -> Vec2 { s.cmp.center + offset_of(c, k) };
        let max_zoom = (one * 4.0).max(2.0);
        if let (Some(h), true) = (hovered, resp.hovered() && !single_ref) {
            let (pinch, swipe, alt) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta, i.modifiers.alt));
            if let Some(t) = crate::zoom::scroll_source(ui.ctx()) {
                self.scroll_trackpad = t;
            }
            let mut pan_swipe = Vec2::ZERO;
            let factor = if self.scroll_trackpad && !alt {
                if self.zoom > 1.01 {
                    pan_swipe = swipe;
                }
                pinch
            } else {
                pinch * (1.0 + swipe.y * 0.002)
            };
            if factor != 1.0 {
                let nz = (self.zoom * factor).clamp(1.0, max_zoom);
                if let Some(p) = pointer {
                    let cz = pc::zoom_about(fields[h].rect, fields[h].aspect, self.zoom, center_of(self, c, h), nz, p, fill);
                    self.cmp.center = cz - (center_of(self, c, h) - self.cmp.center);
                }
                self.zoom = nz;
                self.cmp.changed_at = now;
            }
            if pan_swipe != Vec2::ZERO {
                let size = pc::base_size(fields[h].rect.size(), fields[h].aspect, fill) * self.zoom;
                self.cmp.center -= pan_swipe / size;
                self.cmp.changed_at = now;
            }
        }
        // space held: 100 % at the pointer, back on release
        let space = ui.input(|i| i.key_down(egui::Key::Space)) && !ui.ctx().egui_wants_keyboard_input();
        match (space, self.cmp.space, hovered) {
            (true, None, Some(h)) => {
                self.cmp.space = Some((self.zoom, self.cmp.center));
                let nz = one.max(1.0);
                if let Some(p) = pointer {
                    let cz = pc::zoom_about(fields[h].rect, fields[h].aspect, self.zoom, center_of(self, c, h), nz, p, fill);
                    self.cmp.center = cz - (center_of(self, c, h) - self.cmp.center);
                }
                self.zoom = nz;
                self.cmp.changed_at = now;
            }
            (false, Some((z, cz)), _) => {
                self.zoom = z;
                self.cmp.center = cz;
                self.cmp.space = None;
                self.cmp.changed_at = now;
            }
            _ => {}
        }
        // drag: all fields (⇧ or not coupled: only the field under the pointer)
        if resp.drag_started() {
            let shift = ui.input(|i| i.modifiers.shift);
            self.cmp.drag_single = hovered.filter(|_| shift || !c.coupled).map(|h| fields[h].image);
        }
        if resp.dragged() && !c.checks.measure {
            let d = resp.drag_delta();
            let k = self.cmp.drag_single.and_then(|img| fields.iter().position(|f| f.image == img)).or(hovered).unwrap_or(0);
            let size = pc::base_size(fields[k].rect.size(), fields[k].aspect, fill) * self.zoom;
            match self.cmp.drag_single {
                Some(img) => *c.offsets.entry(img).or_insert(Vec2::ZERO) -= d / size,
                None => self.cmp.center -= d / size,
            }
            self.cmp.changed_at = now;
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if resp.hovered() && self.zoom > 1.01 {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
        if c.checks.measure {
            self.compare_measure_drag(c, &resp, &fields, &shifts);
            // switched on without a rectangle yet: a short hint what to do (QA 1.5.0-test.7: nothing happened visibly)
            if c.measure_rect.is_none() {
                let text = crate::tr("Rechteck ziehen: Schärfe je Feld");
                // above the fields (they are painted after this)
                let p = ui.painter().clone().with_layer_id(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("cmp-measure-hint")));
                let g = p.layout_no_wrap(text.to_string(), egui::FontId::proportional(13.0), Color32::WHITE);
                let r = Rect::from_center_size(area.center(), g.size() + Vec2::new(24.0, 14.0));
                p.rect_filled(r, 0.0, Color32::from_black_alpha(200));
                p.galley(r.min + Vec2::new(12.0, 7.0), g, Color32::WHITE);
            }
        }
        if resp.drag_stopped() {
            self.cmp.drag_single = None;
        }
        if resp.clicked() {
            if let Some(h) = hovered {
                if !tournament && c.focus != Some(fields[h].image) {
                    c.pending.push(Cmd::Focus(fields[h].image));
                }
            }
        }
        if resp.double_clicked() {
            if let Some(h) = hovered {
                let nz = if self.zoom > 1.01 { 1.0 } else { one.max(1.0) };
                if let Some(p) = pointer {
                    let cz = pc::zoom_about(fields[h].rect, fields[h].aspect, self.zoom, center_of(self, c, h), nz, p, fill);
                    self.cmp.center = cz - (center_of(self, c, h) - self.cmp.center);
                }
                self.zoom = nz;
                self.cmp.changed_at = now;
            }
        }
        if self.zoom <= 1.001 {
            self.zoom = 1.0;
        }
        // keep the shared centre where at least one field still moves: each field stops at its own edge (its
        // `image_rect` clamps it), the others go on – no field holds the others back (QA 21.11.2026: an HDR at
        // its right edge kept its source from showing its own right edge)
        let coupled: Vec<pc::Coupled> = (0..fields.len())
            .map(|k| pc::Coupled { size: pc::base_size(fields[k].rect.size(), fields[k].aspect, fill) * self.zoom, field: fields[k].rect.size(), offset: offset_of(c, k) })
            .collect();
        self.cmp.center = pc::clamp_shared_center(self.cmp.center, &coupled);
        // shown whole there is nothing to move at "fit"; filled, the crop may be moved
        if self.zoom <= 1.0 && !fill {
            self.cmp.center = Vec2::splat(0.5);
        }
        // the toolbar's zoom group shows the focused field as it is shown (held at its edge)
        let size0 = fit0 * self.zoom;
        let shown0 = pc::clamp_center(self.cmp.center + offset_of(c, fk), size0, fields[fk].rect.size());
        self.pan = (Vec2::splat(0.5) - shown0) * size0;
        self.cmp.last_pan = self.pan;
        self.view_changed_at = self.cmp.changed_at.max(self.view_changed_at.min(now));
        // ── paint
        self.compare_overlays(&ui.ctx().clone(), c);
        let painter = ui.painter_at(area);
        let blink_ref = single_ref && c.ref_view == RefView::Blink && ((now - self.cmp.blink_start) / c.blink.max(0.2) as f64) as i64 % 2 == 0;
        let mut draw_order: Vec<usize> = (0..fields.len()).collect();
        if single_ref {
            // one field: the reference and the candidate on top of each other
            draw_order = match c.ref_view {
                RefView::Blink => vec![if blink_ref { 0 } else { fields.len().saturating_sub(1) }],
                _ => (0..fields.len()).collect(),
            };
            // only the blink animates: wake up at the next switch; wipe and difference change on input only
            if let Some(d) = blink_repaint_after(c.ref_view, now - self.cmp.blink_start, c.blink as f64) {
                ui.ctx().request_repaint_after(d);
            }
        }
        for &k in &draw_order {
            let f = &fields[k];
            let center = if single_ref { center_of(self, c, 0) } else { center_of(self, c, k) };
            let r = pc::image_rect(f.rect, f.aspect, self.zoom, center, c.fit.fill());
            let clip_rect = if single_ref && c.ref_view == RefView::Wipe && k > 0 {
                Rect::from_min_max(egui::pos2(f.rect.left() + f.rect.width() * c.wipe, f.rect.top()), f.rect.max)
            } else {
                f.rect
            };
            let clip = painter.with_clip_rect(clip_rect);
            if k == 0 || !single_ref {
                clip.rect_filled(f.rect, 0.0, lr::IMAGE_BG);
            }
            // a rejected photo: the whole card is dimmed (compare_field_marks)
            let tint = Color32::WHITE;
            match &f.pic {
                Some(Pic::Plain(t)) => {
                    clip.image(t.id(), r, Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0)), tint);
                }
                Some(Pic::Frame(t, fr, src)) => crate::paint_src_quad(&clip, t.id(), *src, fr, r, tint),
                None => {
                    crate::spinner(&clip, f.rect.center(), 6.5, now, 1.0);
                }
            }
            // full resolution over the preview
            if let (Some((fr, _)), Some((s, region, tex))) = (f.frame.as_ref(), self.cmp.details.get(&f.image)) {
                let settings_now = self.entry_of(f.image).map(|i| self.compare_settings(i));
                if settings_now.as_ref() == Some(s) && self.zoom > 1.01 {
                    let src = [region[0] as f32, region[1] as f32, region[2] as f32, region[3] as f32];
                    crate::paint_src_quad(&clip, tex.id(), src, fr, r, tint);
                }
            }
            let full_uv = Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0));
            if single_ref && c.ref_view == RefView::Difference && k > 0 {
                // the difference as grey over both (bright = changed)
                match self.cmp.overlays.get(f.image, OVERLAY_DIFFERENCE) {
                    Some(t) => {
                        clip.image(t.id(), r, full_uv, Color32::WHITE);
                    }
                    None => crate::spinner(&clip, f.rect.center(), 6.5, now, 1.0),
                }
            }
            for (on, kind) in [(c.checks.peaking, OVERLAY_PEAKING), (c.checks.clipping, OVERLAY_CLIPPING)] {
                if let Some(t) = self.cmp.overlays.get(f.image, kind).filter(|_| on && !single_ref) {
                    clip.image(t.id(), r, full_uv, Color32::WHITE);
                }
            }
            if c.checks.focus_point {
                if let Some(h) = self.lib.compare_hint(f.image) {
                    if let Some([x, y, w, hh]) = h.focus_area.or(h.focus.map(|[x, y]| [x - 0.02, y - 0.02, 0.04, 0.04])) {
                        let fr = Rect::from_min_size(r.min + Vec2::new(x * r.width(), y * r.height()), Vec2::new(w * r.width(), hh * r.height()));
                        clip.rect_stroke(fr, 0.0, egui::Stroke::new(1.5, Color32::WHITE), egui::StrokeKind::Outside);
                    }
                }
            }
            if let Some(m) = c.measure_rect {
                let sh = shifts.get(k).copied().unwrap_or(Vec2::ZERO);
                let mr = Rect::from_min_max(r.min + (m.min.to_vec2() + sh) * r.size(), r.min + (m.max.to_vec2() + sh) * r.size());
                clip.rect_stroke(mr, 0.0, egui::Stroke::new(1.0, Color32::from_rgb(230, 190, 90)), egui::StrokeKind::Outside);
            }
        }
        if single_ref && c.ref_view == RefView::Wipe {
            let f = &fields[0];
            let x = f.rect.left() + f.rect.width() * c.wipe;
            painter.line_segment([egui::pos2(x, f.rect.top()), egui::pos2(x, f.rect.bottom())], egui::Stroke::new(1.5, Color32::WHITE));
            let handle = Rect::from_center_size(egui::pos2(x, f.rect.center().y), Vec2::new(14.0, 40.0));
            let h = ui.interact(handle, egui::Id::unique("cmp-wischer"), egui::Sense::drag());
            painter.rect_filled(handle, 0.0, Color32::from_gray(0xe8));
            if h.dragged() {
                c.wipe = ((x + h.drag_delta().x - f.rect.left()) / f.rect.width()).clamp(0.02, 0.98);
            }
        }
        // ── tags, frames, hints and decisions
        for (k, f) in fields.iter().enumerate() {
            if single_ref && k > 0 && c.ref_view != RefView::Wipe {
                continue;
            }
            self.compare_field_marks(ui, c, f, k, single_ref, blink_ref, !tournament);
        }
        // ── full quality and the opened photos behind it
        let visible: Vec<i64> = fields.iter().map(|f| f.image).collect();
        self.compare_cancel_hidden(&visible, now);
        let need_dev = self.compare_develop_fields(c, &visible, now);
        let need_crop = self.compare_details(ui, c, &fields, &shifts, now);
        self.compare_pool(&visible, c.focus, &need_crop, &need_dev);
        if need_dev.len() + need_crop.len() > 0 || self.cmp.develop_pending.is_some() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(150));
        }
    }

    /// Shift of each field against the first (slightly moved burst frames, by their image signature).
    fn compare_shifts(&self, c: &Compare, fields: &[Field]) -> Vec<Vec2> {
        let sig = |img: i64| self.lib.compare_hint(img).and_then(|h| h.signature);
        let base = fields.first().and_then(|f| sig(f.image));
        // a merged photo (HDR) has the geometry of its reference frame, its frames aligned onto it: against its
        // sources (and the other frames of the bracket) the same photo point is the same scene point. The coarse
        // signature (8 × 8 cells) is no measure there: on tripod frames it found shifts of a quarter of the width
        // (QA 21.11.2026), which moved the source's crop at "fit" and kept it from its edge when zoomed.
        let merged = |img: i64| self.lib.is_merged_image(img);
        let base_merged = fields.first().is_some_and(|f| merged(f.image));
        fields
            .iter()
            .enumerate()
            .map(|(k, f)| match (&base, sig(f.image)) {
                // the signature compares brightness: photos of another exposure (a bracket) are not shifted
                (Some(a), Some(b)) if k > 0 && c.coupled && !base_merged && !merged(f.image) && similar_exposure(a, &b) => library::series::signature_shift(a, &b).map_or(Vec2::ZERO, |(x, y)| Vec2::new(x, y)),
                _ => Vec2::ZERO,
            })
            .collect()
    }

    /// Tag, frame of the focused field, golden edge of the reference, zoom label, flag, hint line and decisions.
    #[allow(clippy::too_many_arguments)]
    fn compare_field_marks(&mut self, ui: &mut egui::Ui, c: &mut Compare, f: &Field, k: usize, single_ref: bool, blink_ref: bool, decide: bool) {
        let p = ui.painter().with_clip_rect(f.rect.expand(2.0));
        let row = self.lib.compare_row(f.image);
        let mut tag = c.place(f.image);
        let editing = !self.in_grid() && self.current.as_ref().is_some_and(|x| self.entries[x.index].image == f.image);
        if single_ref {
            let cand = c.candidate().map(|x| c.place(x)).unwrap_or_default();
            let r = c.reference.map(|x| c.place(x)).unwrap_or_default();
            tag = match c.ref_view {
                RefView::Blink if blink_ref => format!("{} {r}", crate::tr("REFERENZ")),
                RefView::Blink => format!("{} {cand} · {}", crate::tr("KANDIDAT"), crate::trf!("blinkt alle {s} s", s = format!("{:.1}", c.blink).replace('.', ","))),
                RefView::Wipe => format!("{} {r} | {} {cand}", crate::tr("REFERENZ"), crate::tr("KANDIDAT")),
                RefView::Difference => format!("{} {r} ↔ {cand} · {}", crate::tr("DIFFERENZ"), crate::tr("Prüfhilfe")),
                RefView::SideBySide => tag,
            };
        }
        // short tag: place and small symbols (suggestion, reference, being edited); the words in the tooltip
        let mut words: Vec<&str> = Vec::new();
        let mut icons: Vec<(lr::Icon, Color32)> = Vec::new();
        if !single_ref {
            if c.reference == Some(f.image) {
                icons.push((lr::Icon::PushPin, lr::GOLD));
                words.push(crate::tr("Referenz"));
            }
            if c.suggested() == Some(f.image) {
                icons.push((lr::Icon::Sparkle, Color32::WHITE));
                words.push(crate::tr("Vorschlag"));
            }
            if editing {
                icons.push((lr::Icon::Sliders, Color32::WHITE));
                words.push(crate::tr("wird bearbeitet"));
            }
        }
        let tag_rect = if single_ref {
            crate::compare_view::tag(&p, f.rect.min + Vec2::new(6.0, 6.0), &tag, false, false, f.rect.width() - 20.0)
        } else {
            let g = p.layout_no_wrap(format!("{tag} ▾"), egui::FontId::monospace(11.0), Color32::WHITE);
            let w = g.size().x + icons.len() as f32 * 17.0 + 14.0;
            let r = Rect::from_min_size(f.rect.min + Vec2::new(6.0, 6.0), Vec2::new(w, g.size().y + 8.0));
            p.rect_filled(r, 0.0, Color32::from_black_alpha(180));
            p.galley(r.min + Vec2::new(7.0, 4.0), g.clone(), Color32::WHITE);
            for (n, (ic, col)) in icons.iter().enumerate() {
                let at = egui::pos2(r.min.x + 7.0 + g.size().x + 4.0 + n as f32 * 17.0, r.center().y);
                lr::paint_icon(&p, Rect::from_min_size(egui::pos2(at.x, at.y - 6.5), Vec2::splat(13.0)), *ic, *col);
            }
            r
        };
        // the ranking: a clear place mark at the top centre of the field
        if let Some(n) = c.rank_of(f.image) {
            let g = p.layout_no_wrap(format!("{n}."), lr::semibold(20.0), Color32::BLACK);
            let size = g.size() + Vec2::new(18.0, 6.0);
            let r = Rect::from_min_size(egui::pos2(f.rect.center().x - size.x / 2.0, f.rect.top() + 6.0), size);
            p.rect_filled(r, 0.0, Color32::from_gray(0xf0));
            p.galley(r.min + Vec2::new(9.0, 3.0), g, Color32::BLACK);
            // "Übernehmen" sets place 1 only; places 2 and 3 keep their rating (no stars)
            let tip = crate::trf!("Platz {n} im Turnier – „Übernehmen“ setzt: {w}", n = n, w = if n == 1 { crate::tr("Auswahl · Titelfoto") } else { crate::tr("abgelehnt") });
            let h = ui.interact(r, egui::Id::unique(("cmp-platz", f.image)), egui::Sense::hover());
            lr::hover_tip(ui, &h, &tip);
        }
        let tag_tip = if words.is_empty() { crate::tr("Feldmenü").to_string() } else { format!("{} · {}", words.join(" · "), crate::tr("Klick: Feldmenü")) };
        if !single_ref {
            let tr = ui.interact(tag_rect, egui::Id::unique(("cmp-schild", f.image)), egui::Sense::click());
            lr::hover_tip(ui, &tr, &tag_tip);
            egui::Popup::menu(&tr).show(|ui| {
                ui.set_min_width(240.0);
                if self.in_grid() && lr::menu_item(ui, crate::tr("In Entwickeln öffnen"), &crate::shortcuts::display("cmp_develop"), true) {
                    c.pending.push(Cmd::Develop(f.image));
                }
                let hold = if c.reference == Some(f.image) { crate::tr("Referenz aufheben") } else { crate::tr("Als Referenz festhalten") };
                if lr::menu_item(ui, hold, &crate::shortcuts::display("cmp_reference"), true) {
                    c.toggle_reference(f.image);
                }
                if lr::menu_item(ui, crate::tr("Aus dem Vergleich nehmen"), crate::tr("⌘-Klick im Streifen"), c.images.len() > 1) {
                    c.pending.push(Cmd::Remove(f.image));
                }
                if lr::menu_item(ui, crate::tr("Im Raster zeigen"), "", true) {
                    c.pending.push(Cmd::ShowInGrid(f.image));
                }
                if c.offsets.contains_key(&f.image) && lr::menu_item(ui, crate::tr("Wieder gekoppelt"), "", true) {
                    c.offsets.remove(&f.image);
                }
            });
        }
        // fitted: a field not yet in full quality says so at the top right (never a soft preview without a word)
        let state = self.cmp.states.get(&f.image).copied().unwrap_or(FieldState::Full);
        // the zoom field's scale: the focused field (the first one while the focus is on no field shown), and only
        // once its state is known to be full – in the first frame the states are not there yet, and the default
        // "full" gave a scale from the library preview ("100 %" while every field said "wird berechnet", QA 5)
        let scale_field = c.focus == Some(f.image) || (k == 0 && c.focus.is_none_or(|x| c.field_index(x).is_none()));
        if scale_field {
            let known_full = self.cmp.states.get(&f.image) == Some(&FieldState::Full);
            self.cmp.fit_pct = (f.pixels > 0.0 && known_full).then(|| pc::base_size(f.rect.size(), f.aspect, c.fit.fill()).x * ui.ctx().pixels_per_point() / f.pixels * 100.0);
        }
        if self.zoom <= 1.01 && state != FieldState::Full {
            let (text, tip) = match state {
                FieldState::Offline => (crate::tr("Original offline"), crate::tr("Das Original ist nicht erreichbar: das Feld zeigt die Vorschau der Bibliothek")),
                FieldState::Memory => (crate::tr("Vorschau · Speicher knapp"), crate::tr("Zum Entwickeln dieses Feldes fehlt gerade Speicher; es folgt, sobald ein anderes fertig ist")),
                FieldState::Fast => (crate::tr("schnelle Vorschau"), crate::tr("Die genaue Rechnung dieses Fotos ist gescheitert: das Feld zeigt die schnelle Vorschau mit mehr Rauschen")),
                _ => (crate::tr("wird berechnet"), crate::tr("Das Feld wird mit seinen Einstellungen in voller Qualität entwickelt; bis dahin die Vorschau der Bibliothek")),
            };
            let g = p.layout_no_wrap(text.to_string(), egui::FontId::monospace(10.5), Color32::WHITE);
            let spin = state == FieldState::Computing;
            let w = g.size().x + 12.0 + if spin { 16.0 } else { 0.0 };
            let r = Rect::from_min_size(egui::pos2(f.rect.right() - w - 8.0, f.rect.top() + 6.0), Vec2::new(w, g.size().y + 8.0));
            p.rect_filled(r, 0.0, Color32::from_black_alpha(190));
            if spin {
                crate::spinner(&p, egui::pos2(r.left() + 11.0, r.center().y), 4.5, ui.input(|i| i.time), 1.0);
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(60));
            }
            p.galley(egui::pos2(r.right() - g.size().x - 6.0, r.top() + 4.0), g, if state == FieldState::Offline { Color32::from_rgb(0xe6, 0xa2, 0x3c) } else { Color32::WHITE });
            let h = ui.interact(r, egui::Id::unique(("cmp-zustand", f.image)), egui::Sense::hover());
            lr::hover_tip(ui, &h, tip);
        }
        // zoom label at the top right: "100 % · Vorschau …" until the crop at full resolution is there
        if self.zoom > 1.01 {
            // the scale of this photo on screen (fields of other cameras have their own 100 %)
            let pct = if f.pixels > 0.0 { (pc::base_size(f.rect.size(), f.aspect, c.fit.fill()).x * self.zoom * ui.ctx().pixels_per_point() / f.pixels * 100.0).round() } else { (self.zoom / self.one_to_one.max(1e-3) * 100.0).round() };
            let full = self.cmp.details.get(&f.image).is_some_and(|(s, _, _)| self.entry_of(f.image).is_some_and(|i| self.compare_settings(i) == *s));
            let state = if full {
                crate::tr("voll").to_string()
            } else if state == FieldState::Offline {
                crate::tr("Original offline").to_string()
            } else if state == FieldState::Full && f.pic.is_some() && pct <= 100.0 * f.preview_px / f.pixels.max(1.0) {
                // the development covers this scale: as sharp as it gets
                crate::tr("voll").to_string()
            } else if self.cmp.pool_refused && f.frame.is_none() {
                crate::tr("Vorschau · Speicher knapp").to_string()
            } else {
                format!("{} …", crate::tr("Vorschau"))
            };
            let single = if c.offsets.contains_key(&f.image) { format!("{} · ", crate::tr("einzeln")) } else { String::new() };
            let text = format!("{single}{pct:.0} % · {state}");
            let g = p.layout_no_wrap(text, egui::FontId::monospace(10.5), Color32::WHITE);
            let r = Rect::from_min_size(egui::pos2(f.rect.right() - g.size().x - 20.0, f.rect.top() + 6.0), g.size() + Vec2::new(12.0, 8.0));
            p.rect_filled(r, 0.0, Color32::from_black_alpha(170));
            if c.offsets.contains_key(&f.image) {
                // a dashed frame: this field is moved on its own
                let s = egui::Stroke::new(1.0, Color32::from_gray(0xd0));
                for seg in egui::Shape::dashed_line(&[r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()], s, 3.0, 3.0) {
                    p.add(seg);
                }
            }
            p.galley(r.min + Vec2::new(6.0, 4.0), g, Color32::WHITE);
        }
        if c.checks.exif {
            self.compare_exif_line(ui, &c.fields, f.image, f.rect);
        }
        if single_ref {
            return;
        }
        // the foot: stars, hints as small symbols, ✓ ✗ – one row, the card's own ground
        let card = Rect::from_min_max(f.rect.min, egui::pos2(f.rect.max.x, f.rect.max.y + FOOT_H));
        let foot = Rect::from_min_max(egui::pos2(f.rect.left(), f.rect.bottom()), card.max);
        self.compare_foot(ui, c, f.image, foot, row.as_ref(), decide && c.mode != Mode::Tournament);
        // the state colours the whole card: rejected dimmed, picked a thin green line on top
        let flag = row.as_ref().map_or(0, |z| z.flag);
        let pp = ui.painter();
        if flag == -1 {
            pp.rect_filled(card, 0.0, Color32::from_black_alpha(120));
        }
        if flag == 1 {
            pp.rect_filled(Rect::from_min_max(card.min, egui::pos2(card.max.x, card.min.y + 2.0)), 0.0, Color32::from_rgb(0x5c, 0xb8, 0x6a));
        }
        if c.reference == Some(f.image) {
            pp.rect_filled(Rect::from_min_max(egui::pos2(card.left(), card.bottom() - 2.0), card.max), 0.0, lr::GOLD);
        }
        if c.focus == Some(f.image) {
            pp.rect_stroke(card.expand(2.0), 0.0, egui::Stroke::new(2.0, Color32::from_gray(0xe8)), egui::StrokeKind::Inside);
        }
        let _ = k;
    }

    /// Foot of a field card: stars on the left (clickable), the hints as small symbols with their value in the middle
    /// (explanations in the tooltips), ✓ and ✗ on the right.
    fn compare_foot(&mut self, ui: &mut egui::Ui, c: &mut Compare, image: i64, foot: Rect, row: Option<&library::db::Row>, decide: bool) {
        let p = ui.painter().with_clip_rect(foot);
        p.rect_filled(foot, 0.0, CARD);
        let (stars, flag) = row.map_or((0, 0), |z| (z.stars, z.flag));
        let cy = foot.center().y;
        let mut x = foot.left() + 8.0;
        let mut right = foot.right() - 6.0;
        if decide {
            // ✓ ✗ at the right edge
            for (f, icon, tip, key) in [(-1i8, lr::Icon::XCircle, "Als abgelehnt markieren", "reject"), (1i8, lr::Icon::CheckCircle, "Als Auswahl markieren", "pick")] {
                let r = Rect::from_center_size(egui::pos2(right - 10.0, cy), Vec2::splat(20.0));
                let resp = ui.interact(r, egui::Id::unique(("cmp-fuss-flagge", image, f)), egui::Sense::click());
                let on = flag == f;
                let col = if on { if f == 1 { Color32::from_rgb(0x6c, 0xd0, 0x7a) } else { Color32::from_rgb(0xe0, 0x70, 0x60) } } else if resp.hovered() { Color32::WHITE } else { Color32::from_gray(0x80) };
                lr::paint_icon(&p, Rect::from_center_size(r.center(), Vec2::splat(15.0)), icon, col);
                lr::tip_key(ui, &resp, tip, key);
                if resp.clicked() {
                    c.pending.push(Cmd::Flag(image, if on { 0 } else { f }));
                }
                right -= 22.0;
            }
            // stars on the left, small
            for n in 1..=5u8 {
                let r = Rect::from_min_size(egui::pos2(x, cy - 9.0), Vec2::new(13.0, 18.0));
                let resp = ui.interact(r, egui::Id::unique(("cmp-fuss-stern", image, n)), egui::Sense::click());
                let on = n <= stars;
                let col = if on { Color32::from_rgb(0xe8, 0xc5, 0x5a) } else if resp.hovered() { Color32::from_gray(0xc0) } else { Color32::from_gray(0x6a) };
                p.text(r.center(), egui::Align2::CENTER_CENTER, if on { "★" } else { "☆" }, egui::FontId::proportional(11.5), col);
                let tip = if n == 1 { crate::tr("1 Stern").to_string() } else { crate::trf!("{n} Sterne", n = n) };
                lr::hover_tip(ui, &resp, &format!("{tip} ({n})"));
                if resp.clicked() {
                    c.pending.push(Cmd::Stars(image, if stars == n { 0 } else { n }));
                }
                x += 13.0;
            }
            x += 12.0;
        }
        let hint = self.lib.compare_hint(image).unwrap_or_default();
        let measured = c.measure_rect.and_then(|_| self.cmp.measure_values.get(&image).copied());
        let best = self.cmp.measure_best == Some(image);
        let floor = self.compare_motion_floor(c);
        compare_hint_symbols(ui, &p, egui::pos2(x, cy), right - 8.0, &hint, c.sharp_of(image), image, measured, best, floor);
    }

    /// One line in the field with capture time (hundredths), shutter, aperture, ISO, focal length; values that differ
    /// from the majority in amber, dotted underline.
    fn compare_exif_line(&self, ui: &egui::Ui, among: &[i64], image: i64, rect: Rect) {
        let rows: Vec<library::db::Row> = among.iter().filter_map(|&b| self.lib.compare_row(b)).collect();
        let Some(z) = self.lib.compare_row(image) else { return };
        let parts = exif_parts(&z);
        let majority = |k: usize| -> String {
            let mut counts: HashMap<String, usize> = HashMap::new();
            for r in &rows {
                *counts.entry(exif_parts(r)[k].clone()).or_insert(0) += 1;
            }
            counts.into_iter().max_by_key(|x| x.1).map(|x| x.0).unwrap_or_default()
        };
        let p = ui.painter().with_clip_rect(rect);
        let bar = Rect::from_min_max(egui::pos2(rect.left(), rect.bottom() - 22.0), rect.max);
        p.rect_filled(bar, 0.0, Color32::from_black_alpha(170));
        let mut x = bar.left() + 8.0;
        let amber = Color32::from_rgb(0xe6, 0xa2, 0x3c);
        for (k, t) in parts.iter().enumerate() {
            if t.is_empty() {
                continue;
            }
            let differs = k > 0 && rows.len() > 1 && *t != majority(k);
            let r = p.text(egui::pos2(x, bar.center().y), egui::Align2::LEFT_CENTER, t, egui::FontId::monospace(10.5), if differs { amber } else { Color32::from_gray(0xe0) });
            if differs {
                let s = egui::Stroke::new(1.0, amber);
                for seg in egui::Shape::dotted_line(&[egui::pos2(r.left(), r.bottom()), egui::pos2(r.right(), r.bottom())], amber, 3.0, 0.7) {
                    p.add(seg);
                }
                let _ = s;
            }
            x = r.right() + 10.0;
        }
    }

    /// Tournament: progress, decisions below the two photos.
    fn compare_duel_head(&mut self, ui: &mut egui::Ui, c: &mut Compare, area: Rect) {
        let Some(t) = c.tournament.as_ref() else { return };
        // ties add duels: the estimate never falls below the duel shown
        let duels = t.duels() + 1;
        let estimate = t.estimate().max(duels);
        // the tournament's own photos (the selection may have changed meanwhile)
        let out = t.entrants.iter().filter(|&&b| t.is_out(b)).count();
        let entrants = t.entrants.len();
        let head = Rect::from_min_size(area.min, Vec2::new(area.width(), 28.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(head).layout(egui::Layout::left_to_right(egui::Align::Center)));
        let ui = &mut child;
        ui.spacing_mut().item_spacing.x = 10.0;
        ui.label(egui::RichText::new(crate::trf!("Duell {a} von etwa {b} · Ziel Platz 1–3", a = duels, b = estimate)).font(lr::semibold(12.0)).color(Color32::from_gray(0xe0)));
        let (bar, _) = ui.allocate_exact_size(Vec2::new(160.0, 4.0), egui::Sense::hover());
        ui.painter().rect_filled(bar, 0.0, Color32::from_gray(0x30));
        ui.painter().rect_filled(Rect::from_min_size(bar.min, Vec2::new(bar.width() * (duels as f32 / estimate as f32).min(1.0), bar.height())), 0.0, Color32::from_gray(0xd0));
        ui.label(egui::RichText::new(crate::trf!("{a} Fotos · {b} raus", a = entrants, b = out)).color(lr::TEXT_DIM()));
    }

    /// Faces bar: one row per person with its face in every photo – where someone blinks or looks away.
    fn compare_faces_bar(&mut self, ui: &mut egui::Ui, c: &Compare, area: Rect) {
        let persons = self.lib.compare_persons(&c.images);
        let p = ui.painter_at(area);
        let g = lr::caps(&p, crate::tr("Gesichter"), lr::semibold(10.5), Color32::from_gray(0xb0));
        p.galley(area.min + Vec2::new(8.0, 3.0), g, Color32::from_gray(0xb0));
        let size = 58.0;
        for (row, &(cx, cy)) in persons.iter().take(3).enumerate() {
            let y = area.top() + 20.0 + row as f32 * (size + 6.0);
            for (k, &b) in c.images.iter().enumerate() {
                let x = area.left() + 8.0 + k as f32 * (size + 4.0);
                if x + size > area.right() {
                    break;
                }
                let cell = Rect::from_min_size(egui::pos2(x, y), Vec2::new(size, size));
                p.rect_filled(cell, 0.0, Color32::from_gray(0x18));
                let dist = |q: &library::series_quality::Face| (q.x + q.w / 2.0 - cx).powi(2) + (q.y + q.h / 2.0 - cy).powi(2);
                let face = self.lib.compare_hint(b).and_then(|h| h.faces).and_then(|f| f.iter().min_by(|a, b| dist(a).total_cmp(&dist(b))).copied()).filter(|f| dist(f) < 0.15 * 0.15);
                let Some(face) = face else { continue };
                let Some(t) = self.lib.compare_texture(b) else { continue };
                let ts = t.size_vec2();
                let side = (face.w * ts.x).max(face.h * ts.y) * 1.3;
                let fc = egui::pos2(face.x + face.w / 2.0, face.y + face.h / 2.0);
                let uv = Rect::from_center_size(fc, Vec2::new(side / ts.x, side / ts.y));
                p.image(t.id(), cell, uv, Color32::WHITE);
                if face.eyes_closed {
                    p.rect_stroke(cell, 0.0, egui::Stroke::new(1.5, Color32::from_rgb(220, 120, 90)), egui::StrokeKind::Inside);
                }
                let h = ui.interact(cell, egui::Id::unique(("cmp-gesicht", row, k)), egui::Sense::click());
                let tip = match (face.eyes_closed, face.quality) {
                    (true, _) => crate::trf!("{k}: Augen zu", k = c.place(b)),
                    (false, Some(q)) => crate::trf!("{k}: Gesichtsqualität {q}", k = c.place(b), q = format!("{q:.2}")),
                    _ => c.place(b),
                };
                lr::hover_tip(ui, &h, &tip);
            }
        }
    }

    // ───────────────────────────────────────────── Measuring (V2)

    fn compare_measure_drag(&mut self, c: &mut Compare, resp: &egui::Response, fields: &[Field], shifts: &[Vec2]) {
        let Some(p) = resp.interact_pointer_pos() else { return };
        let Some(k) = fields.iter().position(|f| f.rect.contains(p)) else { return };
        let r = pc::image_rect(fields[k].rect, fields[k].aspect, self.zoom, self.cmp.center + shifts[k], c.fit.fill());
        let uv = ((p - r.min) / r.size()).clamp(Vec2::ZERO, Vec2::splat(1.0)) - shifts[k];
        if resp.drag_started() {
            c.measure_rect = Some(Rect::from_min_max(uv.to_pos2(), uv.to_pos2()));
            self.cmp.measure_start = Some(uv.to_pos2());
        } else if resp.dragged() {
            if let Some(a) = self.cmp.measure_start {
                c.measure_rect = Some(Rect::from_two_pos(a, uv.to_pos2()));
            }
        }
        if resp.drag_stopped() {
            self.cmp.measure_start = None;
            self.compare_measure_values(c);
        }
    }

    /// Sharpness in the measuring rectangle of every photo (on its large preview, the same rectangle after the
    /// alignment of the fields); the sharpest gets "schärfstes".
    fn compare_measure_values(&mut self, c: &Compare) {
        self.cmp.measure_values.clear();
        self.cmp.measure_best = None;
        let Some(m) = c.measure_rect.filter(|m| m.width() > 0.005 && m.height() > 0.005) else { return };
        for &b in &c.fields {
            if let Some(v) = self.lib.compare_sharpness(b, [m.min.x, m.min.y, m.max.x, m.max.y]) {
                self.cmp.measure_values.insert(b, v);
            }
        }
        let max = self.cmp.measure_values.values().copied().fold(0.0f32, f32::max);
        if max > 0.0 {
            for v in self.cmp.measure_values.values_mut() {
                *v = *v / max * 100.0;
            }
            self.cmp.measure_best = self.cmp.measure_values.iter().max_by(|a, b| a.1.total_cmp(b.1)).map(|x| *x.0);
        }
    }

    // ───────────────────────────────────────────── Full resolution

    /// Keep the photos of the visible fields opened (the focused one first; at most four, two in the operating mode
    /// "Saving"): one preload at a time through the app's preloading, protected in its cache.
    /// The pool: the focused photo, then the photos still missing their crop (zoomed) or their development – at most
    /// four (two in "Saving"); a photo done leaves the pool, so the next one can be opened in its place.
    fn compare_pool(&mut self, visible: &[i64], focus: Option<i64>, need_crop: &[i64], need_dev: &[i64]) {
        let limit = if crate::perf_mode::plan().cache_limit <= 2 { 2 } else { pc::MAX_FIELDS };
        // a photo whose crop or development is on its way stays opened until it is back (its result is only taken
        // while the photo is opened; dropping it made two photos take turns forever – measured with 8 fields at 100 %)
        let busy: Vec<i64> = self.cmp.pending.as_ref().map(|p| p.0).into_iter().chain(self.cmp.develop_pending.as_ref().map(|p| p.0)).chain(self.cmp.base_pending.as_ref().map(|p| p.0)).collect();
        let order = pool_order(visible, focus, &busy, &[need_crop, need_dev].concat(), limit);
        self.cmp.pool = order.iter().filter_map(|&b| self.entry_of(b)).collect();
        if self.cmp.opening.is_some_and(|i| !self.preload_running.contains(&i)) {
            self.cmp.opening = None;
        }
        let wanted: Vec<usize> = self.cmp.pool.clone();
        for &i in &wanted {
            let have = self.current.as_ref().is_some_and(|c| c.index == i) || self.cache.iter().any(|(k, _)| *k == i) || self.loading == Some(i);
            if have {
                continue;
            }
            if !self.preload_running.is_empty() {
                return;
            }
            let e = &self.entries[i];
            if !crate::image_kind::of(&e.path).editable() || !crate::developable_state(e.state) {
                continue;
            }
            // the focused photo is always opened; the others only while there is room
            if Some(i) != wanted.first().copied() && !crate::mem_budget::room_ahead("compare: opening a photo", crate::mem_budget::OPEN_BYTES) {
                self.cmp.pool_refused = true;
                return;
            }
            self.cmp.pool_refused = false;
            self.ensure_entry(i);
            self.preload_running.push(i);
            self.cmp.opening = Some(i);
            let cancel = self.prefetch_flag(i);
            crate::log(&format!("compare: opening {} for the field", self.entries[i].name));
            self.worker.send(crate::jobs::Job::Prefetch { index: i, path: self.entries[i].path.clone(), settings: self.entries[i].settings.clone(), cancel });
            return;
        }
    }

    /// Request crops at full resolution of the visible part, one at a time, the focused field first, once the view
    /// rests; draw-ready crops are kept per photo (the others' are dropped).
    /// Returns the photos that must be opened for their crop (the focused first).
    fn compare_details(&mut self, ui: &egui::Ui, c: &Compare, fields: &[Field], shifts: &[Vec2], now: f64) -> Vec<i64> {
        let images: Vec<i64> = fields.iter().map(|f| f.image).collect();
        self.cmp.details.retain(|k, _| images.contains(k));
        self.cmp.waiting_since.retain(|k, _| images.contains(k));
        let mut need = Vec::new();
        if self.zoom <= 1.01 {
            self.cmp.waiting_since.clear();
            return need;
        }
        let ppp = ui.ctx().pixels_per_point();
        let settled = now - self.cmp.changed_at > 0.25 && self.cmp.pending.is_none() && now >= self.cmp.blocked_until;
        let mut order: Vec<usize> = (0..fields.len()).collect();
        order.sort_by_key(|&k| Some(fields[k].image) != c.focus);
        let mut busy = self.cmp.pending.is_some();
        for k in order {
            let f = &fields[k];
            // no frame yet: the photo is being opened for its development (asked for there)
            let Some((frame, [fw, fh])) = f.frame.as_ref() else {
                busy = true;
                continue;
            };
            let center = self.cmp.center + shifts[k] + c.offsets.get(&f.image).copied().unwrap_or(Vec2::ZERO);
            let r = pc::image_rect(f.rect, f.aspect, self.zoom, center, c.fit.fill());
            // beyond the preview's resolution only
            if r.width() * ppp <= f.preview_px * 1.1 {
                continue;
            }
            self.cmp.waiting_since.entry(f.image).or_insert(now);
            let vis = r.intersect(f.rect);
            let to_src = |p: Pos2| {
                let u = (p - r.min) / r.size();
                frame.to_src([u.x, u.y])
            };
            let pts = [vis.left_top(), vis.right_top(), vis.right_bottom(), vis.left_bottom()].map(to_src);
            let (x0, x1) = pts.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p[0]), b.max(p[0])));
            let (y0, y1) = pts.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p[1]), b.max(p[1])));
            let region = crate::darkroom_loupe::source_region([x0.max(0.0).floor(), y0.max(0.0).floor(), x1.min(*fw as f32).ceil(), y1.min(*fh as f32).ceil()], [*fw, *fh]);
            let Some(i) = self.entry_of(f.image) else { continue };
            let settings = self.compare_settings(i);
            let have = self.cmp.details.get(&f.image).is_some_and(|(s, dr, _)| *s == settings && crate::contains(*dr, region));
            if have {
                continue;
            }
            busy = true;
            let doc = if let Some(cur) = self.current.as_ref().filter(|x| x.index == i) { Some((cur.loaded.session.clone(), cur.loaded.previews.foreign.clone())) } else { self.cache.iter().find(|(x, _)| *x == i).map(|(_, l)| (l.session.clone(), l.previews.foreign.clone())) };
            // the crop is missing and the photo is not opened (any more): open it (the pool, one after the other)
            let Some((session, foreign)) = doc else {
                need.push(f.image);
                continue;
            };
            // a crop of a photo whose own pipeline is too rough to be shown comes from the exact base only (asked for
            // by the field's development); until then the field keeps what it shows
            if !self.compare_base_given_up(f.image, i) && self.compare_loaded(i).is_some_and(|l| crate::preview_hold::waits_for_base(l, &settings)) {
                continue;
            }
            if !settled {
                continue;
            }
            let req = crate::region_with_margin(region, 0.25, [*fw, *fh]);
            // memory: crops stay below 48 MP each
            if req[2] * req[3] > 48_000_000 {
                continue;
            }
            let generation = self.cmp.generation.load(Ordering::SeqCst);
            self.cmp.pending = Some((f.image, settings.clone(), req, now));
            self.worker.send(crate::jobs::Job::RenderRegion { index: i, session, foreign, settings, region: req, factor: 1, generation, current: self.cmp.generation.clone(), edit: crate::jobs::EditStamp::none() });
            break;
        }
        if busy {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
        need
    }

    /// A crop at full resolution arrived: the comparison's? (`true`: taken.)
    pub(crate) fn compare_take_region(&mut self, ctx: &egui::Context, index: usize, settings: &Settings, region: [usize; 4], width: usize, height: usize, rgb: &[u8]) -> bool {
        let Some(image) = self.entries.get(index).map(|e| e.image) else { return false };
        let ours = self.cmp.pending.as_ref().is_some_and(|(b, s, r, _)| *b == image && s == settings && *r == region);
        if !ours {
            return false;
        }
        let (_, _, _, sent) = self.cmp.pending.take().unwrap();
        let now = ctx.input(|i| i.time);
        let tex = ctx.load_texture(format!("cmp-detail-{image}"), egui::ColorImage::from_rgb([width, height], rgb), crate::DETAIL_OPTIONS);
        self.cmp.details.insert(image, (settings.clone(), region, tex));
        let waited = self.cmp.waiting_since.remove(&image).map_or(0.0, |t| now - t);
        crate::log(&format!("compare: full resolution {} {}×{} – job {:.0} ms, since 100 % {:.0} ms, {} crops ({:.0} MB)", self.entries[index].name, width, height, (now - sent) * 1000.0, waited * 1000.0, self.cmp.details.len(), self.cmp.details.values().map(|(_, r, _)| r[2] * r[3] * 4).sum::<usize>() as f64 / 1e6));
        ctx.request_repaint();
        true
    }

    /// A crop failed: the comparison's? (then a short pause before the next)
    pub(crate) fn compare_region_failed(&mut self, ctx: &egui::Context, index: usize, superseded: bool) -> bool {
        let Some(image) = self.entries.get(index).map(|e| e.image) else { return false };
        if self.cmp.pending.as_ref().is_some_and(|(b, ..)| *b == image) {
            self.cmp.pending = None;
            if !superseded {
                self.cmp.blocked_until = ctx.input(|i| i.time) + 2.0;
            }
            return true;
        }
        false
    }
}

/// How much more directional the edges of a photo must be than in the calmest photo of the comparison to call it
/// "verwischt".
const MOTION_MARGIN: f32 = 0.2;

impl App {
    /// The lowest motion value of the compared photos (a photo alone is never "blurred" against itself).
    fn compare_motion_floor(&self, c: &Compare) -> f32 {
        let v: Vec<f32> = c.images.iter().filter_map(|&b| self.lib.compare_hint(b).and_then(|h| h.quality).map(|q| q.motion)).collect();
        if v.len() < 2 { 1.0 } else { v.into_iter().fold(1.0, f32::min) }
    }
}

fn start_tournament(c: &mut Compare) {
    // from the ranking of a finished one: a new tournament (the ranking gives way)
    if c.ranking_shown() {
        discard_ranking(c);
    }
    if c.tournament.as_ref().is_none_or(|t| t.entrants != c.images || t.done) && c.images.len() >= 2 {
        c.tournament = Some(Tournament::new(c.images.clone()));
    }
    c.mode = Mode::Tournament;
}

/// Tooltip of "Übernehmen" on the ranking: what it sets (one ⌘Z).
fn adopt_tip() -> String {
    crate::tr("Platz 1 wird Auswahl und Titelfoto, alle anderen Fotos des Turniers werden abgelehnt (auch Platz 2 und 3); Sterne bleiben, wie sie sind – ein ⌘Z nimmt alles zurück").to_string()
}

/// The ranking is dropped: nothing is set, the comparison stays with the arrangement of before.
fn discard_ranking(c: &mut Compare) {
    c.tournament = None;
    c.leave_ranking();
    c.mode = Mode::Compare;
}

/// Capture time with hundredths, shutter, aperture, ISO, focal length of a row.
fn exif_parts(z: &library::db::Row) -> [String; 5] {
    let time = z.capture.as_deref().and_then(|c| c.get(11..)).unwrap_or("").to_string();
    let shutter = z.time.map(|t| if t >= 1.0 { format!("{t:.1} s").replace(".0 s", " s") } else { format!("1/{:.0}", 1.0 / t.max(1e-6)) }).unwrap_or_default();
    let ap = z.aperture.map(|a| format!("f/{a:.1}").replace(".0", "").replace('.', ",")).unwrap_or_default();
    let iso = z.iso.map(|i| format!("ISO {i}")).unwrap_or_default();
    let focal = z.focal.map(|f| format!("{f:.0} mm")).unwrap_or_default();
    [time, shutter, ap, iso, focal]
}

// ───────────────────────────────────────────── Inspection overlays (V2) and difference (V3)

/// Kind of an overlay over a field.
pub(crate) const OVERLAY_PEAKING: u8 = 0;
pub(crate) const OVERLAY_CLIPPING: u8 = 1;
pub(crate) const OVERLAY_DIFFERENCE: u8 = 2;

/// Overlays computed in the background on the previews (1024 px): edges above one threshold shared by all fields,
/// clipped highlights and shadows, the difference of the candidate to the reference.
#[derive(Default)]
pub struct Overlays {
    textures: HashMap<(i64, u8), TextureHandle>,
    /// what the textures were made for (the fields of the peaking set, the reference of the difference)
    peaking_set: Vec<i64>,
    difference_ref: Option<i64>,
    rx: Option<std::sync::mpsc::Receiver<Vec<((i64, u8), egui::ColorImage)>>>,
}

impl Overlays {
    pub fn get(&self, image: i64, kind: u8) -> Option<&TextureHandle> {
        self.textures.get(&(image, kind))
    }
}

/// Edges (Laplacian magnitude of the brightness) per photo, marked above one threshold for all photos: the 99.5 %
/// point of the sharpest photo × 0.4 – less green then really means less sharp.
pub(crate) fn peaking_overlays(images: &[(i64, image::RgbImage)]) -> Vec<(i64, egui::ColorImage)> {
    let maps: Vec<(i64, usize, usize, Vec<f32>)> = images
        .iter()
        .map(|(b, img)| {
            let (w, h) = (img.width() as usize, img.height() as usize);
            let l: Vec<f32> = img.pixels().map(|p| 0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32).collect();
            let mut m = vec![0.0f32; w * h];
            for y in 1..h.saturating_sub(1) {
                for x in 1..w.saturating_sub(1) {
                    let i = y * w + x;
                    m[i] = (4.0 * l[i] - l[i - 1] - l[i + 1] - l[i - w] - l[i + w]).abs();
                }
            }
            (*b, w, h, m)
        })
        .collect();
    let p995 = |m: &[f32]| {
        let mut v: Vec<f32> = m.iter().copied().step_by(7).collect();
        v.sort_by(|a, b| a.total_cmp(b));
        v.get(v.len() * 995 / 1000).copied().unwrap_or(0.0)
    };
    let threshold = maps.iter().map(|(_, _, _, m)| p995(m)).fold(0.0f32, f32::max) * 0.4;
    maps.into_iter()
        .map(|(b, w, h, m)| {
            let px: Vec<Color32> = m.iter().map(|&v| if v > threshold.max(4.0) { Color32::from_rgba_unmultiplied(80, 255, 90, 220) } else { Color32::TRANSPARENT }).collect();
            (b, egui::ColorImage::new([w, h], px))
        })
        .collect()
}

/// Clipped highlights (a channel at the top: red) and shadows (all channels at the bottom: blue).
pub(crate) fn clipping_overlay(img: &image::RgbImage) -> egui::ColorImage {
    let px: Vec<Color32> = img
        .pixels()
        .map(|p| {
            if p.0.iter().any(|&v| v >= 250) {
                Color32::from_rgba_unmultiplied(255, 40, 40, 200)
            } else if p.0.iter().all(|&v| v <= 3) {
                Color32::from_rgba_unmultiplied(40, 110, 255, 200)
            } else {
                Color32::TRANSPARENT
            }
        })
        .collect();
    egui::ColorImage::new([img.width() as usize, img.height() as usize], px)
}

/// Difference of the candidate to the reference as grey (bright = changed), the candidate moved by `shift` (0..1)
/// onto the reference; only an aid – rotation and sub-pixel shifts remain.
pub(crate) fn difference_image(reference: &image::RgbImage, candidate: &image::RgbImage, shift: Vec2) -> egui::ColorImage {
    let (w, h) = (reference.width(), reference.height());
    let cand = if candidate.dimensions() == (w, h) { candidate.clone() } else { image::imageops::resize(candidate, w, h, image::imageops::FilterType::Triangle) };
    let (dx, dy) = ((shift.x * w as f32).round() as i64, (shift.y * h as f32).round() as i64);
    let luma = |p: &image::Rgb<u8>| 0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32;
    let mut px = Vec::with_capacity((w * h) as usize);
    for y in 0..h as i64 {
        for x in 0..w as i64 {
            let (cx, cy) = (x + dx, y + dy);
            let v = if cx >= 0 && cy >= 0 && cx < w as i64 && cy < h as i64 { (luma(reference.get_pixel(x as u32, y as u32)) - luma(cand.get_pixel(cx as u32, cy as u32))).abs() * 4.0 } else { 0.0 };
            px.push(Color32::from_gray(v.min(255.0) as u8));
        }
    }
    egui::ColorImage::new([w as usize, h as usize], px)
}

impl App {
    /// Overlays wanted by the inspection aids and the difference: taken when ready, computed in the background
    /// (one run at a time, quiet priority).
    fn compare_overlays(&mut self, ctx: &egui::Context, c: &Compare) {
        if let Some(rx) = &self.cmp.overlays.rx {
            if let Ok(list) = rx.try_recv() {
                for (key, img) in list {
                    let tex = ctx.load_texture(format!("cmp-overlay-{}-{}", key.0, key.1), img, egui::TextureOptions::LINEAR);
                    self.cmp.overlays.textures.insert(key, tex);
                }
                self.cmp.overlays.rx = None;
            }
        }
        let fields = c.fields.clone();
        let diff = (c.reference.is_some() && c.ref_view == RefView::Difference).then(|| c.reference.zip(c.candidate())).flatten();
        // drop what is no longer wanted
        let peaking = c.checks.peaking;
        let clipping = c.checks.clipping;
        if !peaking || self.cmp.overlays.peaking_set != fields {
            self.cmp.overlays.textures.retain(|k, _| k.1 != OVERLAY_PEAKING);
        }
        self.cmp.overlays.textures.retain(|k, _| match k.1 {
            OVERLAY_CLIPPING => clipping && fields.contains(&k.0),
            OVERLAY_DIFFERENCE => diff.is_some_and(|(r, cand)| cand == k.0 && Some(r) == self.cmp.overlays.difference_ref),
            _ => true,
        });
        if self.cmp.overlays.rx.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        }
        let have = |k: (i64, u8)| self.cmp.overlays.textures.contains_key(&k);
        let want_peak = peaking && fields.iter().any(|&b| !have((b, OVERLAY_PEAKING)));
        let want_clip: Vec<i64> = if clipping { fields.iter().copied().filter(|&b| !have((b, OVERLAY_CLIPPING))).collect() } else { Vec::new() };
        let want_diff = diff.filter(|(_, cand)| !have((*cand, OVERLAY_DIFFERENCE)));
        if !want_peak && want_clip.is_empty() && want_diff.is_none() {
            return;
        }
        let src = |b: i64| self.lib.compare_preview_source(b).map(|s| (b, s));
        let peak_src: Vec<(i64, crate::library_ui::PreviewSource)> = if want_peak { fields.iter().filter_map(|&b| src(b)).collect() } else { Vec::new() };
        let clip_src: Vec<(i64, crate::library_ui::PreviewSource)> = want_clip.iter().filter_map(|&b| src(b)).collect();
        let diff_src = want_diff.and_then(|(r, cand)| {
            let shift = match (self.lib.compare_hint(r).and_then(|h| h.signature), self.lib.compare_hint(cand).and_then(|h| h.signature)) {
                (Some(a), Some(b)) if similar_exposure(&a, &b) => library::series::signature_shift(&a, &b).map_or(Vec2::ZERO, |(x, y)| Vec2::new(x, y)),
                _ => Vec2::ZERO,
            };
            Some((src(r)?.1, cand, src(cand)?.1, shift))
        });
        if want_peak {
            self.cmp.overlays.peaking_set = fields.clone();
        }
        if let Some((r, ..)) = want_diff {
            self.cmp.overlays.difference_ref = Some(r);
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.cmp.overlays.rx = Some(rx);
        let ctx = ctx.clone();
        let _ = std::thread::Builder::new().name("compare-overlays".into()).spawn(move || {
            library::platform::set_priority(library::platform::Priority::Utility);
            let mut out: Vec<((i64, u8), egui::ColorImage)> = Vec::new();
            if !peak_src.is_empty() {
                let imgs: Vec<(i64, image::RgbImage)> = peak_src.iter().filter_map(|(b, s)| Some((*b, s.read()?))).collect();
                out.extend(peaking_overlays(&imgs).into_iter().map(|(b, i)| ((b, OVERLAY_PEAKING), i)));
            }
            for (b, s) in clip_src {
                if let Some(img) = s.read() {
                    out.push(((b, OVERLAY_CLIPPING), clipping_overlay(&img)));
                }
            }
            if let Some((r, cand, cs, shift)) = diff_src {
                if let (Some(a), Some(b)) = (r.read(), cs.read()) {
                    out.push(((cand, OVERLAY_DIFFERENCE), difference_image(&a, &b, shift)));
                }
            }
            let _ = tx.send(out);
            ctx.request_repaint();
        });
    }
}

/// Time until the next repaint a reference view needs without any input: only the blink view switches by itself,
/// at multiples of its period since `elapsed` (seconds since the phase started); `None` for every other view.
fn blink_repaint_after(view: RefView, elapsed: f64, period: f64) -> Option<std::time::Duration> {
    if view != RefView::Blink {
        return None;
    }
    let period = period.max(0.2);
    let rest = period - elapsed.max(0.0).rem_euclid(period);
    // a hair late, so the frame lands behind the switch; never a busy loop
    Some(std::time::Duration::from_secs_f64(rest + 0.002).max(std::time::Duration::from_millis(5)))
}

#[cfg(test)]
mod overlay_tests {
    use super::*;

    /// Only blinking repaints by itself, and at the next switch instead of every 50 ms.
    #[test]
    fn repaint_only_for_blink_at_the_next_switch() {
        assert_eq!(blink_repaint_after(RefView::Wipe, 3.0, 1.0), None);
        assert_eq!(blink_repaint_after(RefView::Difference, 3.0, 1.0), None);
        assert_eq!(blink_repaint_after(RefView::SideBySide, 3.0, 1.0), None);
        let d = blink_repaint_after(RefView::Blink, 2.25, 1.0).unwrap().as_secs_f64();
        assert!((d - 0.752).abs() < 1e-6, "{d}");
        let d = blink_repaint_after(RefView::Blink, 0.0, 0.1).unwrap().as_secs_f64();
        assert!((0.2..=0.21).contains(&d), "{d}"); // period floor 0.2 s
    }

    #[test]
        fn peaking_uses_one_threshold_for_all() {
        let sharp = image::RgbImage::from_fn(32, 32, |x, _| if x % 2 == 0 { image::Rgb([0, 0, 0]) } else { image::Rgb([255, 255, 255]) });
        let soft = image::RgbImage::from_fn(32, 32, |x, _| image::Rgb([(x * 8) as u8; 3]));
        let o = peaking_overlays(&[(1, sharp), (2, soft)]);
        let marked = |img: &egui::ColorImage| img.pixels.iter().filter(|p| p.a() > 0).count();
        assert!(marked(&o[0].1) > 100 && marked(&o[1].1) == 0, "{} {}", marked(&o[0].1), marked(&o[1].1));
    }

    /// "Vergleich muss immer voll sein": the photos are opened one after the other for the fields still missing their
    /// development or crop, the focused first, never more than the limit and never a photo not shown.
    #[test]
    fn pool_opens_focus_then_missing_fields_within_the_limit() {
        let visible = [1, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(pool_order(&visible, Some(3), &[], &[1, 2, 4, 5], 4), vec![3, 1, 2, 4]);
        // a photo with a job on its way stays before the missing ones; a photo done leaves the pool, the next takes
        // its place
        assert_eq!(pool_order(&visible, Some(3), &[6], &[2, 4], 4), vec![3, 6, 2, 4]);
        assert_eq!(pool_order(&visible, Some(3), &[6], &[2, 4], 2), vec![3, 6]);
        assert_eq!(pool_order(&visible, Some(3), &[], &[8], 2), vec![3, 8]);
        // nothing missing: only the focused photo
        assert_eq!(pool_order(&visible, Some(3), &[], &[], 4), vec![3]);
        // a photo no longer shown (another page) is never opened
        assert_eq!(pool_order(&[1, 2], Some(9), &[9], &[2], 4), vec![2]);
    }

    /// Every button acts or is greyed with its reason: align without a field moved on its own, coupled with one
    /// field, the arrangement in the tournament.
    #[test]
    fn view_buttons_say_why_they_cannot_act() {
        let mut c = Compare::new(None, None);
        c.follow(vec![1, 2], Some(1));
        let st = view_states(&c, &[]);
        assert!(st[0].0 && st[1].0 && st[3].0);
        assert!(!st[2].0 && st[2].1.is_some(), "align without offsets");
        c.offsets.insert(2, Vec2::new(0.1, 0.0));
        assert!(view_states(&c, &[]).iter().all(|x| x.0));
        c.mode = Mode::Tournament;
        assert!(!view_states(&c, &[]).first().unwrap().0, "fixed arrangement in the tournament");
        let mut one = Compare::new(None, None);
        one.follow(vec![1], Some(1));
        let st = view_states(&one, &[Arrange::Row, Arrange::Column, Arrange::Square, Arrange::FocusLarge, Arrange::All]);
        assert!(!st[0].0 && !st[1].0, "one field: nothing to arrange or couple");
        // G skips arrangements showing the same fields
        assert_eq!(next_distinct(Arrange::Auto, &[Arrange::Row]), Arrange::Column);
    }

    #[test]
    fn difference_is_dark_where_equal() {
        let a = image::RgbImage::from_fn(16, 16, |x, y| image::Rgb([(x * 10) as u8, (y * 10) as u8, 0]));
        let d = difference_image(&a, &a, Vec2::ZERO);
        assert!(d.pixels.iter().all(|p| p.r() == 0));
    }
}

/// The arrangements in the menu.
const ARRANGEMENTS: [(Arrange, &str); 6] = [
    (Arrange::Auto, "Automatisch: gleich große Felder, so groß wie möglich"),
    (Arrange::Row, "Eine Reihe"),
    (Arrange::Column, "Eine Spalte"),
    (Arrange::Square, "2 × 2"),
    (Arrange::FocusLarge, "Gewähltes groß, die anderen klein daneben"),
    (Arrange::All, "Alle"),
];

/// Groups of the comparison's bar (`App::compare_toolbar`), in the order they give way.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tool {
    Check,
    Reference,
    View,
    Decide,
    Mode,
}

fn check_text(on: bool, text: &str) -> String {
    format!("{}  {text}", if on { "✓" } else { "  " })
}

fn toggle_check(c: &mut Compare, i: usize) {
    match i {
        0 => c.checks.focus_point = !c.checks.focus_point,
        1 => c.checks.peaking = !c.checks.peaking,
        2 => c.checks.clipping = !c.checks.clipping,
        3 => c.checks.exif = !c.checks.exif,
        4 => {
            c.checks.measure = !c.checks.measure;
            if !c.checks.measure {
                c.measure_rect = None;
            }
        }
        _ => c.faces_bar = !c.faces_bar,
    }
}

/// Photos the comparison keeps opened, in the order they are opened: the focused one (when shown), then those with a
/// job on its way, then those still missing their crop or development – only shown ones, at most `limit`.
fn pool_order(visible: &[i64], focus: Option<i64>, busy: &[i64], need: &[i64], limit: usize) -> Vec<i64> {
    let mut order: Vec<i64> = focus.filter(|f| visible.contains(f)).into_iter().collect();
    for &b in busy.iter().chain(need) {
        if visible.contains(&b) && !order.contains(&b) {
            order.push(b);
        }
    }
    order.truncate(limit);
    order
}

/// Name of an arrangement in the menu; "Alle" with the number of photos and the page size ("Alle (7)", "Alle (40, je
/// 8)").
fn arrange_name(a: Arrange, text: &str, photos: usize) -> String {
    if a != Arrange::All {
        return crate::tr(text).to_string();
    }
    if photos > pc::MAX_ALL {
        crate::trf!("Alle ({n}, je {k})", n = photos, k = pc::MAX_ALL)
    } else {
        crate::trf!("Alle ({n})", n = photos)
    }
}

/// The next arrangement for G that changes the fields (arrangements giving the same fields are skipped).
fn next_distinct(a: Arrange, same: &[Arrange]) -> Arrange {
    let mut n = a.next();
    for _ in 0..5 {
        if !same.contains(&n) {
            return n;
        }
        n = n.next();
    }
    a
}

/// Arrangement, coupled, align, fit: can each act now, and if not, why (tooltip).
fn view_states(c: &Compare, same: &[Arrange]) -> [(bool, Option<&'static str>); 4] {
    let tour = c.mode == Mode::Tournament;
    let arrange = if tour {
        (false, Some("Im Turnier stehen die zwei Fotos immer nebeneinander"))
    } else if ARRANGEMENTS.iter().all(|(a, _)| *a == c.arrange || same.contains(a)) {
        (false, Some("Bei einem Feld gibt es nichts anzuordnen"))
    } else {
        (true, None)
    };
    let single = c.fields.len() < 2;
    let coupled = if single { (false, Some("Gekoppelt wirkt erst ab zwei Feldern")) } else { (true, None) };
    let align = if c.offsets.is_empty() { (false, Some("Ausrichten: kein Feld ist einzeln verschoben (⇧-Ziehen)")) } else { (true, None) };
    [arrange, coupled, align, (true, None)]
}

impl App {
    /// Focus point, edges, clipping, capture, measure, faces: can each act now, and if not, why (tooltip). A switch that
    /// is on can always be switched off.
    fn check_states(&self, c: &Compare) -> [(bool, Option<&'static str>); 6] {
        let focus = c.checks.focus_point || c.fields.iter().any(|&b| self.lib.compare_hint(b).is_some_and(|h| h.focus.is_some() || h.focus_area.is_some()));
        let measure = c.checks.measure || c.fields.len() >= 2;
        // without faces the bar shows nothing: greyed with the reason, also when it is switched on (the setting stays)
        let faces = !self.lib.compare_persons(&c.images).is_empty();
        [
            if focus { (true, None) } else { (false, Some("Fokuspunkt: in diesen Dateien nicht gespeichert")) },
            (true, None),
            (true, None),
            (true, None),
            if measure { (true, None) } else { (false, Some("Messen vergleicht die Schärfe mehrerer Felder – erst zwei Fotos auswählen")) },
            if faces { (true, None) } else { (false, Some("Gesichterleiste: in diesen Fotos keine Gesichter gefunden")) },
        ]
    }
}

/// A menu that opens above `anchor` (the bar is at the bottom), toggled with `egui::Popup::toggle_id`.
fn popup_above(ui: &egui::Ui, id: egui::Id, anchor: Rect, f: impl FnOnce(&mut egui::Ui)) {
    egui::Popup::new(id, ui.ctx().clone(), anchor, ui.layer_id()).open_memory(None).align(egui::RectAlign::TOP_START).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        ui.set_min_width(240.0);
        f(ui)
    });
}

/// Mean brightness of two image signatures within 15 % (the shift between them can be trusted).
fn similar_exposure(a: &[u8], b: &[u8]) -> bool {
    let mean = |s: &[u8]| s.iter().skip(8).step_by(3).map(|&v| v as f32).sum::<f32>() / (s.len().saturating_sub(8) / 3).max(1) as f32;
    let (x, y) = (mean(a), mean(b));
    (x - y).abs() <= 0.15 * x.max(y).max(1.0)
}

/// The hints of a photo as small symbols with their value, starting at `at`, never beyond `limit` (explanations in
/// the tooltips): sharpness as four bars, measured sharpness, eyes, clipped highlights, motion blur, focus beside
/// the faces.
#[allow(clippy::too_many_arguments)]
fn compare_hint_symbols(ui: &egui::Ui, p: &egui::Painter, at: Pos2, limit: f32, hint: &pc::Hint, sharp: Option<f32>, image: i64, measured: Option<f32>, best: bool, motion_floor: f32) {
    let cy = at.y;
    let mut x = at.x;
    let amber = Color32::from_rgb(220, 180, 110);
    let dim = Color32::from_gray(0xa8);
    let font = egui::FontId::proportional(10.5);
    let item = |x: &mut f32, icon: Option<lr::Icon>, text: String, col: Color32, tip: &str, n: usize| {
        let w_text = if text.is_empty() { 0.0 } else { p.layout_no_wrap(text.clone(), font.clone(), col).size().x + 3.0 };
        let w = if icon.is_some() { 15.0 } else { 0.0 } + w_text;
        if *x + w > limit {
            return;
        }
        let r = Rect::from_min_size(egui::pos2(*x, cy - 9.0), Vec2::new(w, 18.0));
        if let Some(ic) = icon {
            lr::paint_icon(p, Rect::from_center_size(egui::pos2(*x + 6.5, cy), Vec2::splat(12.0)), ic, col);
        }
        if !text.is_empty() {
            p.text(egui::pos2(*x + if icon.is_some() { 15.0 } else { 0.0 }, cy), egui::Align2::LEFT_CENTER, text, font.clone(), col);
        }
        let h = ui.interact(r, egui::Id::unique(("cmp-hinweis", image, n)), egui::Sense::hover());
        lr::hover_tip(ui, &h, tip);
        *x += w + 10.0;
    };
    let Some(q) = hint.quality else {
        item(&mut x, None, "…".into(), lr::TEXT_DIM(), crate::tr("wird gemessen …"), 0);
        return;
    };
    if let Some(v) = measured {
        let tip = if best { format!("{} {v:.0} · {}", crate::tr("Schärfe"), crate::tr("schärfstes")) } else { format!("{} {v:.0}", crate::tr("Schärfe")) };
        item(&mut x, Some(lr::Icon::Ruler), format!("{v:.0}"), if best { lr::GOLD } else { dim }, &tip, 1);
    }
    // sharpness: four small bars
    let filled = sharp.map_or(0, |s| ((s * 4.0).round() as usize).clamp(1, 4));
    if x + 30.0 <= limit {
        let bars = Rect::from_min_size(egui::pos2(x, cy - 5.0), Vec2::new(4.0 * 6.0, 10.0));
        for b in 0..4 {
            let r = Rect::from_min_size(egui::pos2(x + b as f32 * 6.0, cy - 5.0), Vec2::new(4.0, 10.0));
            if b < filled {
                p.rect_filled(r, 0.0, Color32::from_gray(0xd8));
            } else {
                p.rect_stroke(r, 0.0, egui::Stroke::new(1.0, Color32::from_gray(0x60)), egui::StrokeKind::Inside);
            }
        }
        let h = ui.interact(bars, egui::Id::unique(("cmp-schaerfe", image)), egui::Sense::hover());
        lr::hover_tip(ui, &h, &crate::trf!("Schärfe: {a} % des schärfsten Fotos der Auswahl (gemessen an der schärfsten Stelle)", a = (sharp.unwrap_or(0.0) * 100.0).round() as u32));
        x = bars.right() + 10.0;
    }
    if let (Some(faces), Some(closed)) = (q.faces, q.eyes_closed) {
        if faces > 0 {
            let tip = crate::tr("Gesichter und Augen (Apple Vision); geschlossene Augen sind im Foto umrandet");
            if closed == 0 {
                item(&mut x, Some(lr::Icon::Eye), format!("{faces}/{faces}"), dim, tip, 2);
            } else {
                item(&mut x, Some(lr::Icon::EyeSlash), format!("{closed}/{faces}"), Color32::from_rgb(220, 140, 110), tip, 2);
            }
        }
    }
    if q.clip_high > 0.005 {
        item(&mut x, Some(lr::Icon::Sun), format!("{:.0} %", (q.clip_high * 100.0).max(1.0)), amber, crate::tr("Anteil abgeschnittener Lichter in der Vorschau"), 3);
    }
    if q.motion > 0.6 && q.motion - motion_floor > MOTION_MARGIN {
        item(&mut x, Some(lr::Icon::ArrowsH), String::new(), amber, crate::tr("Die Kanten sind in einer Richtung verwischt (Bewegungsunschärfe)"), 4);
    }
    if hint.focus_beside_faces() {
        item(&mut x, Some(lr::Icon::Crosshair), String::new(), amber, crate::tr("Das Fokusfeld der Kamera berührt keines der gefundenen Gesichter (nur ein Hinweis)"), 5);
    }
}
