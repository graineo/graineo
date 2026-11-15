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
    });
    // Keys only for the topmost dialog (a confirmation dialog above another one takes them alone)
    if !r.is_top_modal {
        return answer;
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
                ui.label(egui::RichText::new("↩").small().color(lr::TEXT_DIM));
            }
        }
        if ui.add(egui::Button::new(crate::tr(k.cancel)).min_size(egui::vec2(90.0, BUTTON_H)).corner_radius(0.0).fill(Color32::TRANSPARENT).stroke(egui::Stroke::new(1.0, crate::lr::FIELD_STROKE))).clicked() {
            a = Answer::Cancel;
        }
        ui.label(egui::RichText::new(crate::tr("Esc")).small().color(lr::TEXT_DIM));
    });
    a
}

/// Short text (one or two sentences).
pub fn text(ui: &mut egui::Ui, t: &str) {
    ui.add(egui::Label::new(egui::RichText::new(crate::menu::i18n::tr_msg(t)).color(lr::TEXT)).wrap());
}

/// Grey hint line (small).
pub fn hint(ui: &mut egui::Ui, t: &str) {
    ui.add(egui::Label::new(egui::RichText::new(crate::menu::i18n::tr_msg(t)).small().color(lr::TEXT_DIM)).wrap());
}

/// "Is being determined …": calm turning spinner and the text beside it (redraws a few times a second, only while shown).
pub fn pending(ui: &mut egui::Ui, t: &str) {
    ui.horizontal(|ui| {
        crate::activity::calm_spinner(ui, 12.0, lr::TEXT_DIM);
        ui.add(egui::Label::new(egui::RichText::new(crate::menu::i18n::tr_msg(t)).small().color(lr::TEXT_DIM)).wrap());
    });
}

/// Yellow warning line (small), e.g. not enough space.
pub fn warning(ui: &mut egui::Ui, t: &str) {
    ui.add(egui::Label::new(egui::RichText::new(crate::menu::i18n::tr_msg(t)).small().color(Color32::from_rgb(0xe0, 0xb0, 0x40))).wrap());
}

/// Grey chip with key figures, e.g. "184 photos · 2.3 GB".
pub fn chip(ui: &mut egui::Ui, t: &str) {
    egui::Frame::new().fill(Color32::from_gray(0x2a)).corner_radius(0.0).inner_margin(egui::Margin::symmetric(9, 3)).show(ui, |ui| {
        ui.label(egui::RichText::new(crate::tr(t)).small().color(Color32::from_gray(0xc8)));
    });
}

/// Framed option group.
pub fn options<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.add_space(4.0);
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
            if ui.add(egui::Label::new(egui::RichText::new(crate::tr(label)).color(lr::TEXT)).selectable(false).sense(egui::Sense::click())).clicked() {
                *on = !*on;
                changed = true;
            }
        });
        ui.horizontal(|ui| {
            ui.add_space(44.0);
            ui.add(egui::Label::new(egui::RichText::new(crate::tr(locked.unwrap_or(explanation))).small().color(lr::TEXT_DIM)).truncate());
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
    ui.label(egui::RichText::new(crate::tr(label)).small().color(lr::TEXT_DIM));
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
        ui.add(egui::Label::new(egui::RichText::new(crate::menu::i18n::tr_msg(current)).small().color(lr::TEXT_DIM)).truncate());
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
        ui.add(egui::Label::new(egui::RichText::new(crate::menu::i18n::tr_msg(summary)).color(lr::TEXT)).wrap());
    });
    if !details.is_empty() {
        egui::CollapsingHeader::new(egui::RichText::new(crate::trf!("Details ({a})", a = details.len())).small().color(lr::TEXT_DIM)).id_salt("dialog-details").default_open(false).show(ui, |ui| {
            egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                for d in details {
                    ui.add(egui::Label::new(egui::RichText::new(crate::menu::i18n::tr_msg(d)).small().monospace().color(lr::TEXT_DIM)).wrap());
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
    ui.label(egui::RichText::new(r.timing()).small().color(lr::TEXT_DIM));
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
    /// Export dialog: title with count, content as before, "Export" as main action (↩).
    pub(crate) fn export_dialog(&mut self, ctx: &egui::Context) {
        let n = self.export_count;
        let title = if n == 1 { crate::tr("1 Bild exportieren").to_string() } else { crate::trf!("{a} exportieren", a = count(n, crate::tr("Bild"), crate::tr("Bilder"))) };
        let mut items = Vec::new();
        // an earlier target inside an Apple Photos library or another program's package is never suggested
        if self.export_dir.as_deref().is_some_and(|d| library::write_target::check(d).is_err()) {
            self.export_dir = None;
        }
        let can = !self.exporting && self.export_dir.is_some();
        let a = show(ctx, "export", &title, 620.0, |ui| items = self.export_controls(ui, true), Buttons::confirm(crate::tr("Exportieren"), can && n > 0));
        self.export_count = items.len();
        match a {
            Answer::Primary if !items.is_empty() => {
                self.export(items);
                self.show_export = false;
            }
            Answer::Cancel => self.show_export = false,
            _ => {}
        }
    }
}
