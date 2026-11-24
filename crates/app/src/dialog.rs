//! One dialog component for all dialogs of the app (same structure as the settings window).
//!
//! Structure from top to bottom (fixed width, 20 px margin, clear spacing):
//! 1. **Title** – short, bold, as a statement or question with the key figure ("Delete 184 rejected photos?")
//! 2. **Text** – one or two sentences on the consequence; key figure/amount as a grey chip ([`chip`])
//! 3. **Options** (only when needed) – framed group, per row a toggle + short label and
//!    below it ONE grey explanation line (with live number); whatever does not apply is disabled with a reason
//! 4. Separator, below it the **buttons** right-aligned, same height: "Cancel" on the left, the main
//!    action on the right. Destructive action outlined in red and named after the action ("Move to Trash").
//!    ↩ = main action (not for destructive ones), Esc always cancels; hints small and grey.
//!
//! The dialog dims the background and is modal. Variants: confirmation, destructive
//! confirmation, input/selection, progress ([`progress`]), result ([`outcome`]).

use eframe::egui;
use egui::Color32;

use crate::lr;
use crate::menu::i18n::{self,Text};

pub const WIDTH: f32 = 440.0;
const MARGIN: f32 = 20.0;
const BUTTON_H: f32 = 26.0;
const RED: Color32 = Color32::from_rgb(0xe0, 0x5c, 0x50);

/// Style of the main action.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Style {
    Normal,
    /// destructive (red), ↩ does not trigger it
    Destructive,
}

/// Buttons bottom right. `primary = None`: only one button (e.g. "Close" as `cancel`).
pub struct Buttons<'a> {
    pub cancel: &'a str,
    pub primary: Option<(&'a str, Style, bool)>,
}

impl<'a> Buttons<'a> {
    pub fn confirm(primary: &'a str, active: bool) -> Self {
        Buttons { cancel: i18n::text(Text::Cancel), primary: Some((primary, Style::Normal, active)) }
    }
    pub fn destructive(primary: &'a str, active: bool) -> Self {
        Buttons { cancel: i18n::text(Text::Cancel), primary: Some((primary, Style::Destructive, active)) }
    }
    pub fn close() -> Self {
        Buttons { cancel: i18n::text(Text::Close), primary: None }
    }
}

/// What the user did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Answer {
    Open,
    Primary,
    Cancel,
}

/// Show the dialog. `content` draws text, chips, options (with the helpers below).
pub fn show(ctx: &egui::Context, id: &str, title: &str, width: f32, content: impl FnOnce(&mut egui::Ui), buttons: Buttons) -> Answer {
    show_extra(ctx, id, title, width, content, buttons, None).0
}

/// Like [`show`] with one more plain button left of the others (e.g. "Looks ansehen" beside "Fertig" in a result);
/// returns whether that one was clicked. An empty `cancel` text leaves the cancel button out (Esc still answers
/// [`Answer::Cancel`]).
pub fn show_extra(ctx: &egui::Context, id: &str, title: &str, width: f32, content: impl FnOnce(&mut egui::Ui), buttons: Buttons, extra: Option<&str>) -> (Answer, bool) {
    let mut extra_clicked = false;
    let frame = egui::Frame::new().fill(Color32::from_gray(0x1e)).stroke(egui::Stroke::new(1.0, Color32::from_gray(0x3a))).corner_radius(0.0).inner_margin(MARGIN);
    let mut answer = Answer::Open;
    let r = egui::Modal::new(egui::Id::unique(("dialog", id))).frame(frame).backdrop_color(Color32::from_black_alpha(150)).show(ctx, |ui| {
        ui.set_width(width);
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.add(egui::Label::new(egui::RichText::new(crate::tr(title)).font(lr::semibold(15.0)).color(Color32::from_gray(0xee))).wrap());
        ui.add_space(6.0);
        lr::bounded(ui, content);
        ui.add_space(10.0);
        ui.separator();
        ui.add_space(6.0);
        answer = button_row(ui, &buttons);
        if let Some(t) = extra {
            // left of the row's buttons (drawn after them in the right-to-left row)
            let row = ui.min_rect();
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(egui::Rect::from_min_max(egui::pos2(row.left(), row.bottom() - BUTTON_H), row.max)).layout(egui::Layout::left_to_right(egui::Align::Center)));
            extra_clicked = child.add(egui::Button::new(crate::tr(t)).min_size(egui::vec2(90.0, BUTTON_H)).corner_radius(0.0).fill(Color32::TRANSPARENT).stroke(egui::Stroke::new(1.0, crate::lr::FIELD_STROKE()))).clicked();
        }
    });
    // Keys only for the topmost dialog (a confirmation dialog above another one takes them alone)
    if !r.is_top_modal {
        return (answer, extra_clicked);
    }
    // Esc always cancels; ↩ only for a non-destructive main action (and not while typing multi-line)
    if answer == Answer::Open && (r.should_close() || ctx.input(|i| i.key_pressed(egui::Key::Escape))) {
        answer = Answer::Cancel;
    }
    if answer == Answer::Open {
        if let Some((_, Style::Normal, true)) = buttons.primary {
            if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                answer = Answer::Primary;
            }
        }
    }
    (answer, extra_clicked)
}

/// Part of a [`show_scrolling`] dialog the content closure draws.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Part {
    /// the content (scrolls when the window is too low)
    Body,
    /// fixed lines above the buttons (always visible)
    Footer,
    /// controls at the right end of the title row (e.g. a template choice that applies to the whole dialog)
    Header,
}

/// Space between a dialog and the window edge.
const SCREEN_GAP: f32 = 24.0;

/// Like [`show`] for long dialogs: the body scrolls when the window is too low, the footer (`footer_h` high) and the
/// buttons always stay at the bottom; the dialog is never higher than the window. ↩ triggers the main action only
/// when no text field had the keyboard (in a text field ↩ just confirms the field).
pub fn show_scrolling(ctx: &egui::Context, id: &str, title: &str, width: f32, footer_h: f32, mut content: impl FnMut(&mut egui::Ui, Part), buttons: Buttons) -> Answer {
    let typing = ctx.egui_wants_keyboard_input();
    let max_h = (ctx.content_rect().height() - 2.0 * SCREEN_GAP - 2.0 * MARGIN).max(160.0);
    let frame = egui::Frame::new().fill(Color32::from_gray(0x1e)).stroke(egui::Stroke::new(1.0, Color32::from_gray(0x3a))).corner_radius(0.0).inner_margin(MARGIN);
    let mut answer = Answer::Open;
    let r = egui::Modal::new(egui::Id::unique(("dialog", id))).frame(frame).backdrop_color(Color32::from_black_alpha(150)).show(ctx, |ui| {
        ui.set_width(width);
        ui.spacing_mut().item_spacing.y = 6.0;
        let top = ui.cursor().top();
        ui.horizontal(|ui| {
            ui.add(egui::Label::new(egui::RichText::new(crate::tr(title)).font(lr::semibold(15.0)).color(Color32::from_gray(0xee))).truncate());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| content(ui, Part::Header));
        });
        ui.add_space(4.0);
        // below the body: space, separator, space, footer, space, buttons (with the item spacing between them)
        let spacing = ui.spacing().item_spacing.y;
        let below = 8.0 + 1.0 + 4.0 + footer_h + 4.0 + BUTTON_H + 6.0 * spacing;
        let body_h = (max_h - (ui.cursor().top() - top) - below).max(60.0);
        egui::ScrollArea::vertical().id_salt(("dialog-body", id)).max_height(body_h).auto_shrink([false, true]).show(ui, |ui| {
            lr::bounded(ui, |ui| content(ui, Part::Body));
        });
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(4.0);
        ui.allocate_ui_with_layout(egui::vec2(width, footer_h), egui::Layout::top_down(egui::Align::Min), |ui| {
            ui.set_min_size(egui::vec2(width, footer_h));
            content(ui, Part::Footer);
        });
        ui.add_space(4.0);
        answer = button_row(ui, &buttons);
    });
    if !r.is_top_modal {
        return answer;
    }
    if answer == Answer::Open && (r.should_close() || ctx.input(|i| i.key_pressed(egui::Key::Escape))) {
        answer = Answer::Cancel;
    }
    if answer == Answer::Open && !typing {
        if let Some((_, Style::Normal, true)) = buttons.primary {
            if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                answer = Answer::Primary;
            }
        }
    }
    answer
}

fn button_row(ui: &mut egui::Ui, k: &Buttons) -> Answer {
    let mut a = Answer::Open;
    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), BUTTON_H), egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        if let Some((text, style, active)) = k.primary {
            // Main action: gold with dark text; destructive action outlined in red
            let clicked = match style {
                Style::Normal => lr::main_button_min(ui, text, active, 110.0).clicked(),
                Style::Destructive => {
                    let b = egui::Button::new(egui::RichText::new(crate::tr(text)).strong().color(Color32::WHITE)).min_size(egui::vec2(110.0, BUTTON_H)).corner_radius(0.0).fill(Color32::from_rgb(0x5a, 0x22, 0x20)).stroke(egui::Stroke::new(1.0, RED));
                    ui.add_enabled(active, b).clicked()
                }
            };
            if clicked {
                a = Answer::Primary;
            }
            if style == Style::Normal && active {
                ui.label(egui::RichText::new("↩").small().color(lr::TEXT_DIM()));
            }
        }
        if k.cancel.is_empty() {
            return;
        }
        if ui.add(egui::Button::new(crate::tr(k.cancel)).min_size(egui::vec2(90.0, BUTTON_H)).corner_radius(0.0).fill(Color32::TRANSPARENT).stroke(egui::Stroke::new(1.0, crate::lr::FIELD_STROKE()))).clicked() {
            a = Answer::Cancel;
        }
        ui.label(egui::RichText::new(crate::tr("Esc")).small().color(lr::TEXT_DIM()));
    });
    a
}

/// Short text (one or two sentences).
pub fn text(ui: &mut egui::Ui, t: &str) {
    ui.add(egui::Label::new(egui::RichText::new(crate::menu::i18n::tr_msg(t)).color(lr::TEXT())).wrap());
}

/// Grey hint line (small).
pub fn hint(ui: &mut egui::Ui, t: &str) {
    ui.add(egui::Label::new(egui::RichText::new(crate::menu::i18n::tr_msg(t)).small().color(lr::TEXT_DIM())).wrap());
}

/// "Is being determined …": calm turning spinner and the text beside it (redraws a few times a second, only while shown).
pub fn pending(ui: &mut egui::Ui, t: &str) {
    ui.horizontal(|ui| {
        crate::activity::calm_spinner(ui, 12.0, lr::TEXT_DIM());
        ui.add(egui::Label::new(egui::RichText::new(crate::menu::i18n::tr_msg(t)).small().color(lr::TEXT_DIM())).wrap());
    });
}

/// Yellow warning line (small), e.g. not enough space.
pub fn warning(ui: &mut egui::Ui, t: &str) {
    ui.add(egui::Label::new(egui::RichText::new(crate::menu::i18n::tr_msg(t)).small().color(Color32::from_rgb(0xe0, 0xb0, 0x40))).wrap());
}

/// Grey chip with key figures, e.g. "184 photos · 2.3 GB".
pub fn chip(ui: &mut egui::Ui, t: &str) {
    // air above (not below the text before it): a chip as the last element must not add space before the divider
    ui.add_space(8.0);
    egui::Frame::new().fill(Color32::from_gray(0x2a)).corner_radius(0.0).inner_margin(egui::Margin::symmetric(9, 3)).show(ui, |ui| {
        ui.label(egui::RichText::new(crate::tr(t)).small().color(Color32::from_gray(0xc8)));
    });
}

/// Framed option group.
pub fn options<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.add_space(10.0);
    egui::Frame::new()
        .fill(Color32::from_gray(0x19))
        .stroke(egui::Stroke::new(1.0, Color32::from_gray(0x30)))
        .corner_radius(0.0)
        .inner_margin(10.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 4.0;
            add(ui)
        })
        .inner
}

/// One option: toggle + label, below it ONE grey line (always the same height – no jumping).
/// `locked`: reason why the option does not apply (then greyed out, reason instead of explanation).
pub fn option(ui: &mut egui::Ui, on: &mut bool, label: &str, explanation: &str, locked: Option<&str>) -> bool {
    let mut changed = false;
    // disabled: show as off (the stored value stays untouched)
    let mut off = false;
    let on: &mut bool = if locked.is_some() { &mut off } else { on };
    ui.add_enabled_ui(locked.is_none(), |ui| {
        ui.horizontal(|ui| {
            changed = toggle(ui, on);
            if ui.add(egui::Label::new(egui::RichText::new(crate::tr(label)).color(lr::TEXT())).selectable(false).sense(egui::Sense::click())).clicked() {
                *on = !*on;
                changed = true;
            }
        });
        ui.horizontal(|ui| {
            ui.add_space(44.0);
            ui.add(egui::Label::new(egui::RichText::new(crate::tr(locked.unwrap_or(explanation))).small().color(lr::TEXT_DIM())).truncate());
        });
    });
    changed
}

/// Toggle as in the settings.
pub fn toggle(ui: &mut egui::Ui, on: &mut bool) -> bool {
    crate::lr::switch(ui, on)
}

/// Input field (one line) with label above; has focus when opened.
pub fn input(ui: &mut egui::Ui, label: &str, value: &mut String) -> egui::Response {
    ui.label(egui::RichText::new(crate::tr(label)).small().color(lr::TEXT_DIM()));
    let r = crate::lr::edit_field(ui, egui::TextEdit::singleline(value).desired_width(ui.available_width()));
    if !r.has_focus() && ui.memory(|m| m.focused().is_none()) {
        r.request_focus();
    }
    r
}

/// Progress: bar, below it the current item (one line, shortened) – fixed height.
pub fn progress(ui: &mut egui::Ui, fraction: Option<f32>, current: &str) {
    let w = ui.available_width();
    let (r, _) = ui.allocate_exact_size(egui::vec2(w, 6.0), egui::Sense::hover());
    ui.painter().rect_filled(r, 0.0, Color32::from_gray(0x33));
    match fraction {
        Some(a) => {
            let f = egui::Rect::from_min_size(r.min, egui::vec2(w * a.clamp(0.0, 1.0), r.height()));
            ui.painter().rect_filled(f, 0.0, Color32::from_rgb(0x3b, 0x7d, 0xd8));
        }
        None => {
            // indeterminate: moving segment
            let t = ui.input(|i| i.time) as f32;
            let x = r.left() + (t * 0.6).fract() * (w + 80.0) - 80.0;
            let f = egui::Rect::from_x_y_ranges(x.max(r.left())..=(x + 80.0).min(r.right()), r.y_range());
            ui.painter().rect_filled(f, 0.0, Color32::from_rgb(0x3b, 0x7d, 0xd8));
            // 25 frames a second are enough for the moving segment (hours of moving to the NAS must not heat the Mac)
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(40));
        }
    }
    ui.allocate_ui(egui::vec2(w, 16.0), |ui| {
        ui.add(egui::Label::new(egui::RichText::new(crate::menu::i18n::tr_msg(current)).small().color(lr::TEXT_DIM())).truncate());
    });
}

/// Traffic light of a result.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict {
    Green,
    Yellow,
    Red,
}

/// Result: dot + summary, details collapsed.
pub fn outcome(ui: &mut egui::Ui, status: Verdict, summary: &str, details: &[String]) {
    let c = match status {
        Verdict::Green => Color32::from_rgb(0x4c, 0xc2, 0x6a),
        Verdict::Yellow => Color32::from_rgb(0xe0, 0xb0, 0x40),
        Verdict::Red => RED,
    };
    ui.horizontal_top(|ui| {
        let (r, _) = ui.allocate_exact_size(egui::vec2(12.0, 16.0), egui::Sense::hover());
        ui.painter().circle_filled(r.center(), 5.0, c);
        ui.add(egui::Label::new(egui::RichText::new(crate::menu::i18n::tr_msg(summary)).color(lr::TEXT())).wrap());
    });
    if !details.is_empty() {
        egui::CollapsingHeader::new(egui::RichText::new(crate::trf!("Details ({a})", a = details.len())).small().color(lr::TEXT_DIM())).id_salt("dialog-details").default_open(false).show(ui, |ui| {
            egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                for d in details {
                    ui.add(egui::Label::new(egui::RichText::new(crate::menu::i18n::tr_msg(d)).small().monospace().color(lr::TEXT_DIM())).wrap());
                }
            });
        });
    }
}

/// A running job that keeps buttons of a dialog locked (see [`blocked`]).
#[derive(Clone, Debug, Default)]
pub struct Running {
    /// what runs, short and already translated ("Abgleich · Pictures (NAS)")
    pub what: String,
    /// running for
    pub since: std::time::Duration,
    /// share done, when the total is known
    pub fraction: Option<f32>,
    /// duration of the last run of the same kind, when known
    pub last: Option<std::time::Duration>,
}

impl Running {
    /// "läuft seit 1 Min. 5 s · noch etwa 40 s": from the progress, otherwise from the last run.
    pub fn timing(&self) -> String {
        let mut hint = crate::trf!("läuft seit {t}", t = duration(self.since));
        match (self.fraction, self.last) {
            (Some(f), _) if f > 0.02 => hint.push_str(&crate::trf!(" · noch etwa {t}", t = duration(self.since.mul_f32((1.0 - f.min(1.0)) / f)))),
            (_, Some(last)) if last > self.since => hint.push_str(&crate::trf!(" · der letzte Lauf dauerte {t}", t = duration(last))),
            (_, Some(last)) => hint.push_str(&crate::trf!(" · länger als der letzte Lauf ({t})", t = duration(last))),
            _ => hint.push_str(&crate::tr(" · Dauer nicht bekannt")),
        }
        hint
    }
}

/// Explains locked buttons: what runs, why the dialog waits for it (`why`, one sentence), since when and how much
/// longer. Refreshes itself every second.
pub fn blocked(ui: &mut egui::Ui, why: &str, r: &Running) {
    outcome(ui, Verdict::Yellow, &crate::trf!("Gerade läuft: {a}. {b}", a = r.what, b = crate::tr(why)), &[]);
    ui.label(egui::RichText::new(r.timing()).small().color(lr::TEXT_DIM()));
    ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
}

/// "3 Min. 12 s" / "45 s" for waiting hints in dialogs.
pub fn duration(d: std::time::Duration) -> String {
    let s = d.as_secs();
    if s >= 60 {
        crate::trf!("{m} Min. {s} s", m = s / 60, s = s % 60)
    } else {
        crate::trf!("{s} s", s = s)
    }
}

/// Number with thousands separator ("45 337" as in the rest of the app: thin space).
pub fn number(n: usize) -> String {
    i18n::format::integer(i18n::language(),n)
}

/// Size in MB/GB with decimal comma.
pub fn size(bytes: u64) -> String {
    i18n::format::size_decimal(i18n::language(),bytes)
}

/// Plural: "1 photo" / "3 photos".
pub fn count(n: usize, one: &str, many: &str) -> String {
    format!("{} {}", number(n), crate::tr(if n == 1 { one } else { many }))
}

// ───────────────────────────────────────────── App dialogs

impl crate::App {
    /// Export dialog: title with count; two columns when the window is wide enough, the body scrolls, the footer
    /// (name preview, conflicts, progress) and "Export" (↩) stay at the bottom.
    pub(crate) fn export_dialog(&mut self, ctx: &egui::Context) {
        let n = self.export_count;
        let title = if n == 1 { crate::tr("1 Bild exportieren").to_string() } else { crate::trf!("{a} exportieren", a = count(n, crate::tr("Bild"), crate::tr("Bilder"))) };
        // an earlier target inside an Apple Photos library or another program's package is never suggested
        if self.export_dir.as_deref().is_some_and(|d| library::write_target::check(d).is_err()) {
            self.export_dir = None;
        }
        // without a folder the main button chooses one first, then exports
        let can = !self.exporting && self.export_names_ui.pick.is_none();
        let label = if self.export_dir.is_none() { crate::tr("Ordner wählen und exportieren …") } else { crate::tr("Exportieren") };
        let width = (ctx.content_rect().width() - 2.0 * SCREEN_GAP - 2.0 * MARGIN).clamp(340.0, 940.0);
        let two_columns = width >= crate::export_ui::TWO_COLUMNS_MIN;
        let mut scope = crate::export_ui::Scope::default();
        let a = show_scrolling(
            ctx,
            "export",
            &title,
            width,
            crate::export_ui::STATUS_H,
            |ui, part| match part {
                Part::Body => scope = self.export_body(ui, two_columns),
                Part::Footer => self.export_status_lines(ui, &scope),
                // a fixed block at the right end of the title row, filled from the left
                Part::Header => {
                    let w = ui.available_width().min(470.0);
                    ui.allocate_ui_with_layout(egui::vec2(w, 24.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.set_min_width(w);
                        ui.label(egui::RichText::new(crate::tr("Vorlage")).color(lr::TEXT_DIM()));
                        self.export_template_controls(ui);
                    });
                }
            },
            Buttons::confirm(label, can && n > 0),
        );
        let items = scope.items;
        self.export_count = items.len();
        match a {
            Answer::Primary if !items.is_empty() => {
                if self.export_requested(ctx, items) {
                    self.show_export = false;
                }
            }
            Answer::Cancel => {
                self.show_export = false;
                self.export_names_ui.pick = None;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(ctx: &egui::Context, events: Vec<egui::Event>, lines: usize, field: bool) -> (Answer, egui::Rect) {
        let mut answer = Answer::Open;
        let mut footer = egui::Rect::NOTHING;
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0));
        let mut text = String::new();
        let _ = crate::ui_test::run(ctx, egui::RawInput { screen_rect: Some(screen), events, ..Default::default() }, |ui| {
            answer = show_scrolling(
                ui.ctx(),
                "test-scrolling",
                "Test",
                600.0,
                36.0,
                |ui, part| match part {
                    Part::Body => {
                        if field {
                            let r = ui.text_edit_singleline(&mut text);
                            r.request_focus();
                        }
                        for i in 0..lines {
                            ui.label(format!("row {i}"));
                        }
                    }
                    Part::Header => {}
                    Part::Footer => {
                        ui.label("footer");
                        footer = ui.min_rect();
                    }
                },
                Buttons::confirm("OK", true),
            );
        });
        (answer, footer)
    }

    fn key(k: egui::Key) -> egui::Event {
        egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }
    }

    /// A body far higher than the window scrolls; footer and buttons stay inside the window.
    #[test]
    fn long_body_keeps_footer_and_buttons_in_the_window() {
        let ctx = egui::Context::default();
        for _ in 0..3 {
            let (a, footer) = frame(&ctx, vec![], 200, false);
            assert_eq!(a, Answer::Open);
            assert!(footer.is_positive(), "{footer:?}");
            // the buttons sit below the footer: footer + buttons must end above the window's lower edge
            assert!(footer.bottom() + BUTTON_H < 720.0, "{footer:?}");
        }
    }

    /// ↩ triggers the main action, Esc cancels; ↩ in a text field only confirms the field.
    #[test]
    fn enter_and_escape() {
        let ctx = egui::Context::default();
        frame(&ctx, vec![], 3, false);
        assert_eq!(frame(&ctx, vec![key(egui::Key::Enter)], 3, false).0, Answer::Primary);
        assert_eq!(frame(&ctx, vec![key(egui::Key::Escape)], 3, false).0, Answer::Cancel);
        let ctx = egui::Context::default();
        frame(&ctx, vec![], 3, true);
        frame(&ctx, vec![], 3, true);
        assert_eq!(frame(&ctx, vec![key(egui::Key::Enter)], 3, true).0, Answer::Open);
    }
}
