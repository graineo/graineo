//! Controls in the style of common photo apps, designed like
//! a measuring instrument (near-black, IBM Plex Mono, small caps, tick scales): sliders with name/track/value, histogram with
//! clipping triangles, section headers, shortcut overview.

use eframe::egui;
use egui::{Color32, Stroke};

/// Colors (near-black dark theme).
pub const PANEL: Color32 = Color32::from_rgb(0x17, 0x18, 0x1a);
pub const PANEL_DARK: Color32 = Color32::from_rgb(0x0f, 0x10, 0x11);
pub const IMAGE_BG: Color32 = Color32::from_rgb(0x0c, 0x0c, 0x0d);
pub const TEXT: Color32 = Color32::from_gray(0xd0);
pub const TEXT_DIM: Color32 = Color32::from_gray(0xa6);
/// Slider names: brighter than TEXT_DIM so the panels read less grey in grey.
pub const CATEGORY_LABEL: Color32 = Color32::from_gray(0xbc);
pub const SLIDER_LABEL: Color32 = Color32::from_gray(0xc4);
/// Extra vertical space per slider row.
pub const SLIDER_AIR: f32 = 4.0;
/// Grouped controls (variant C): a slightly lighter surface, no frame.
pub const GROUP_FILL: Color32 = Color32::from_gray(0x23);
/// Active choice: lighter surface with a fine light line at the bottom.
pub const ACTIVE_FILL: Color32 = Color32::from_gray(0x3d);
pub const ACTIVE_MARK: Color32 = Color32::from_gray(0xa8);
/// Secondary button surface (no frame) and input fields (darker than the surroundings).
pub const BUTTON_FILL: Color32 = Color32::from_gray(0x2b);
pub const FIELD_FILL: Color32 = Color32::from_gray(0x0f);
pub const FIELD_STROKE: Color32 = Color32::from_gray(0x33);
pub const ACCENT: Color32 = Color32::from_gray(0xb8);
/// Gold tone for "active" and "selected" (underline of the active area, active toggles, selection frame).
/// The app's blue for "on" and "chosen" (switch on, progress bar, chosen mode, links) – never gold.
pub const BLUE: Color32 = Color32::from_rgb(0x3b, 0x7d, 0xd8);
/// Lighter text variant of [`BLUE`] on dark ground (clickable lines).
pub const BLUE_TEXT: Color32 = Color32::from_rgb(0x7f, 0xaa, 0xe6);
pub const GOLD: Color32 = Color32::from_rgb(0xc8, 0xaa, 0x62);

/// Colored tracks as in common photo apps (muted so they don't shout).
pub const GRADIENT_TEMP: [Color32; 2] = [Color32::from_rgb(0x4d, 0x7c, 0xc7), Color32::from_rgb(0xd9, 0xc0, 0x4a)];
pub const GRADIENT_TINT: [Color32; 2] = [Color32::from_rgb(0x5f, 0xb0, 0x5a), Color32::from_rgb(0xc4, 0x5a, 0xb8)];
pub const GRADIENT_SAT: [Color32; 2] = [Color32::from_gray(0x80), Color32::from_rgb(0xd8, 0x6a, 0x3c)];
pub const GRADIENT_RED: [Color32; 2] = [Color32::from_gray(0x50), Color32::from_rgb(0xd0, 0x55, 0x4c)];
pub const GRADIENT_GREEN: [Color32; 2] = [Color32::from_gray(0x50), Color32::from_rgb(0x62, 0xb8, 0x5c)];
pub const GRADIENT_BLUE: [Color32; 2] = [Color32::from_gray(0x50), Color32::from_rgb(0x5a, 0x86, 0xd8)];
pub const GRADIENT_EXPOSURE: [Color32; 2] = [Color32::from_gray(0x2a), Color32::from_gray(0xd0)];

/// Fonts: IBM Plex Mono (embedded, SIL OFL) for the whole interface – labels,
/// values, section headers –, SF Pro (system font) for longer texts such as the help cards.
fn load_fonts(ctx: &egui::Context) {
    use egui::epaint::text::{FontData, FontDefinitions, FontFamily, VariationCoords};
    let mut defs = FontDefinitions::default();
    let defaults = defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let plex = [
        ("plex", &include_bytes!("../assets/fonts/IBMPlexMono-Regular.ttf")[..]),
        ("plex-medium", &include_bytes!("../assets/fonts/IBMPlexMono-Medium.ttf")[..]),
        ("phosphor", &include_bytes!("../assets/fonts/Phosphor-Light.ttf")[..]),
        // Japanese glyphs (Plex Mono has none): IBM Plex Sans JP Regular (SIL OFL), last in every chain
        ("plex-jp", &include_bytes!("../assets/fonts/IBMPlexSansJP-Regular.ttf")[..]),
        // font of the brand (SIL OFL): only for the name "graineo" (top left, see brand.rs)
        ("syne", &include_bytes!("../assets/fonts/Syne-Medium.ttf")[..]),
    ];
    for (name, bytes) in plex {
        defs.font_data.insert(name.to_string(), std::sync::Arc::new(FontData::from_static(bytes)));
    }
    let mut sf = Vec::new();
    if let Ok(bytes) = std::fs::read("/System/Library/Fonts/SFNS.ttf") {
        let mut data = FontData::from_owned(bytes);
        data.tweak.coords = VariationCoords::new([("wght", 440.0)]);
        defs.font_data.insert("sf".to_string(), std::sync::Arc::new(data));
        sf.push("sf".to_string());
    }
    // Windows: Segoe UI as system font, Segoe UI Symbol for stars, flags, arrows (otherwise empty boxes)
    if cfg!(windows) {
        let fonts = std::env::var_os("SystemRoot").map(std::path::PathBuf::from).unwrap_or_else(|| "C:\\Windows".into()).join("Fonts");
        for (name, file) in [("segoe", "segoeui.ttf"), ("segoe-symbol", "seguisym.ttf")] {
            if let Ok(bytes) = std::fs::read(fonts.join(file)) {
                defs.font_data.insert(name.to_string(), std::sync::Arc::new(FontData::from_owned(bytes)));
                sf.push(name.to_string());
            }
        }
    }
    // Plex first, then SF and the egui default for glyphs Plex lacks (arrows, symbols)
    let chain = |first: &str| -> Vec<String> { std::iter::once(first.to_string()).chain(sf.iter().cloned()).chain(defaults.iter().cloned()).chain(std::iter::once("plex-jp".to_string())).collect() };
    defs.families.insert(FontFamily::Proportional, chain("plex"));
    defs.families.insert(FontFamily::Monospace, chain("plex"));
    defs.families.insert(FontFamily::Name("semibold".into()), chain("plex-medium"));
    defs.families.insert(FontFamily::Name("icons".into()), vec!["phosphor".to_string()]);
    defs.families.insert(FontFamily::Name("brand".into()), chain("syne"));
    defs.families.insert(FontFamily::Name("sans".into()), sf.iter().cloned().chain(defaults.iter().cloned()).chain(std::iter::once("plex-jp".to_string())).collect());
    ctx.set_fonts(defs);
}

/// Emphasised font (IBM Plex Mono Medium) for headings and values.
pub fn semibold(size: f32) -> egui::FontId {
    let family = egui::FontFamily::Name("semibold".into());
    egui::FontId::new(size, family)
}

/// Font of the brand (Syne Medium), only for the name of the app.
pub fn brand(size: f32) -> egui::FontId {
    egui::FontId::new(size, egui::FontFamily::Name("brand".into()))
}

/// System font (SF Pro) for body text, e.g. in the help cards.
pub fn sans(size: f32) -> egui::FontId {
    egui::FontId::new(size, egui::FontFamily::Name("sans".into()))
}

/// Base size of the interface (IBM Plex Mono, 11.5 pt).
pub const UI_SIZE: f32 = 11.5;

/// Capitals with letter spacing (section headers, groups), as on a measuring instrument.
pub fn caps(painter: &egui::Painter, text: &str, font: egui::FontId, color: Color32) -> std::sync::Arc<egui::Galley> {
    caps_spaced(painter, text, font, color, 1.1)
}

pub fn caps_spaced(painter: &egui::Painter, text: &str, font: egui::FontId, color: Color32, spacing: f32) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::default();
    job.append(&crate::tr(text).to_uppercase(), 0.0, egui::TextFormat { font_id: font, color, extra_letter_spacing: spacing, ..Default::default() });
    painter.layout_job(job)
}

/// Slider name in capitals (slightly spaced), left-aligned and vertically centred at `at`; returns: text rect.
fn label_caps(painter: &egui::Painter, at: egui::Pos2, text: &str, color: Color32) -> egui::Rect {
    let g = caps_spaced(painter, text, egui::FontId::proportional(UI_SIZE), color, 0.5);
    let r = egui::Align2::LEFT_CENTER.anchor_size(at, g.size());
    painter.galley(r.min, g, color);
    r
}

pub fn apply_theme(ctx: &egui::Context) {
    load_fonts(ctx);
    ctx.options_mut(|o| o.theme_preference = egui::ThemePreference::Dark);
    let mut v = egui::Visuals::dark();
    v.panel_fill = PANEL;
    v.window_fill = Color32::from_rgb(0x1e, 0x1f, 0x22);
    v.extreme_bg_color = PANEL_DARK;
    v.faint_bg_color = Color32::from_rgb(0x1c, 0x1d, 0x1f);
    v.override_text_color = Some(TEXT);
    v.selection.bg_fill = Color32::from_gray(0x68);
    v.selection.stroke = Stroke::new(1.0, Color32::from_gray(0xe0));
    v.widgets.noninteractive.bg_fill = PANEL;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_gray(0x34));
    v.widgets.inactive.bg_fill = BUTTON_FILL;
    v.widgets.inactive.weak_bg_fill = BUTTON_FILL;
    // Basic rule: anything with a fill or a frame is clickable; plain text is a label. Secondary buttons: surface, no frame.
    v.widgets.inactive.bg_stroke = Stroke::NONE;
    v.widgets.hovered.bg_stroke = Stroke::NONE;
    v.widgets.active.bg_stroke = Stroke::NONE;
    // input fields darker than the surroundings with a fine frame
    v.text_edit_bg_color = Some(FIELD_FILL);
    // Mac convention: arrow cursor on controls (hand only on the web); brightening shows clickability
    v.interact_cursor = None;
    v.widgets.hovered.bg_fill = Color32::from_gray(0x36);
    v.widgets.hovered.weak_bg_fill = Color32::from_gray(0x36);
    v.widgets.active.bg_fill = ACTIVE_FILL;
    v.widgets.active.weak_bg_fill = ACTIVE_FILL;
    v.slider_trailing_fill = false;
    // square throughout: buttons, fields, menus, switches, windows and tooltips
    for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = egui::CornerRadius::ZERO;
    }
    v.window_corner_radius = egui::CornerRadius::ZERO;
    v.menu_corner_radius = egui::CornerRadius::ZERO;
    ctx.set_visuals(v);
    ctx.global_style_mut(|s| {
        s.spacing.slider_rail_height = 2.0;
        // the scroll bar gets its own space: content never runs under it
        s.spacing.scroll = egui::style::ScrollStyle::solid();
        s.spacing.scroll.bar_inner_margin = 2.0;
        s.spacing.scroll.bar_outer_margin = 2.0;
        s.spacing.item_spacing = egui::vec2(6.0, 5.0);
        s.spacing.interact_size.y = 18.0;
        // narrow, calm scroll bar that doesn't light up on hover
        // floating: takes no space (otherwise panels whose content depends on the
        // available height – e.g. the filmstrip – grow a little with every frame)
        let mut scroll = egui::style::ScrollStyle::floating();
        scroll.bar_width = 6.0;
        scroll.floating_width = 2.0;
        // Content never touches the bar – a band of [`SCROLL_BAND`] stays free on the right (below for
        // horizontal bars) while the bar is shown: thin bar 1 pt from the edge, 6 pt free before it (2 when it widens
        // under the pointer). Row highlights and right-aligned numbers end before the band.
        scroll.bar_outer_margin = 1.0;
        scroll.floating_allocated_width = SCROLL_BAND;
        scroll.dormant_background_opacity = 0.0;
        scroll.active_background_opacity = 0.0;
        scroll.interact_background_opacity = 0.0;
        scroll.dormant_handle_opacity = 0.25;
        scroll.active_handle_opacity = 0.4;
        scroll.interact_handle_opacity = 0.55;
        s.spacing.scroll = scroll;
        // No hints on mere hover – explanations come via a click on "?"
        s.interaction.tooltip_delay = 1.0e9;
        for (style, size) in [
            (egui::TextStyle::Body, UI_SIZE),
            (egui::TextStyle::Button, UI_SIZE),
            (egui::TextStyle::Small, 10.5),
            (egui::TextStyle::Heading, 13.0),
            (egui::TextStyle::Monospace, UI_SIZE),
        ] {
            if let Some(f) = s.text_styles.get_mut(&style) {
                f.size = size;
            }
        }
    });
}

/// Slider: name, track, value.
/// - Double-click on name or track: default value
/// - Click on the track jumps there, dragging the knob adjusts relatively; with ⌥ ten times finer
/// - Click the value to type it, or drag horizontally
/// - Mouse over the row + ↑/↓ steps the value (⇧ coarse, ⌥ fine)
/// The fill runs from the default value to the current value, so deviations are visible at once.
pub fn slider(ui: &mut egui::Ui, label: &str, value: &mut f64, range: std::ops::RangeInclusive<f64>, default: f64, step: f64) -> bool {
    slider_hint(ui, label, value, range, default, step, "")
}

pub fn slider_hint(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f64,
    range: std::ops::RangeInclusive<f64>,
    default: f64,
    step: f64,
    hint: &str,
) -> bool {
    slider_full(ui, label, value, range, default, step, hint, None, 0.0)
}

/// Marker for [`slider_plain`]: no help card, no tooltip on the name.
const PLAIN: &str = "\u{0}plain";

/// Left text edge of a tool tray (Masks, Retouch): category headings ([`category`]), slider names of
/// [`slider_plain`], notes ([`note`]) and status lines start this far right of the content edge; areas (lists,
/// buttons, row highlights, tracks) start at the content edge itself. Inside a collapsible group ([`section_local`])
/// the content is indented by the group step, so its names line up with the group title.
pub const TEXT_INSET: f32 = 4.0;

/// [`slider`] for tools whose sliders share a name with a slider of the Edit panel but act differently (local masks,
/// removal size …): same operation (double-click resets to `default`), but without the help card of the global slider.
/// The name starts at the text edge of the tray ([`TEXT_INSET`]).
pub fn slider_plain(ui: &mut egui::Ui, label: &str, value: &mut f64, range: std::ops::RangeInclusive<f64>, default: f64, step: f64) -> bool {
    slider_full(ui, label, value, range, default, step, PLAIN, None, TEXT_INSET)
}

/// Dimmed note line of a tool tray (hint, state, count): small, wrapped, at the text edge ([`TEXT_INSET`]).
pub fn note(ui: &mut egui::Ui, text: &str) -> egui::Response {
    egui::Frame::NONE
        .inner_margin(egui::Margin { left: TEXT_INSET as i8, ..Default::default() })
        .show(ui, |ui| ui.add(egui::Label::new(egui::RichText::new(text).small().color(TEXT_DIM)).wrap()))
        .inner
}

/// Width of `ui` that is visible (inside indented sections a wide row before can widen the layout beyond the panel).
pub fn visible_width(ui: &egui::Ui) -> f32 {
    ui.available_width().min(ui.clip_rect().right() - ui.cursor().left() - 4.0).max(40.0)
}

/// [`slider_plain`] for `f32` values (the mask and retouch data).
pub fn slider_plain_f32(ui: &mut egui::Ui, label: &str, value: &mut f32, range: std::ops::RangeInclusive<f32>, default: f32, step: f64) -> bool {
    let mut v = *value as f64;
    // never wider than the visible panel (inside indented sections a wide row before can widen the layout, and the
    // value at the right edge would be clipped)
    let w = visible_width(ui).max(60.0);
    let changed = ui.allocate_ui(egui::vec2(w, 0.0), |ui| {
        ui.set_max_width(w);
        slider_plain(ui, label, &mut v, *range.start() as f64..=*range.end() as f64, default as f64, step)
    }).inner;
    if changed {
        *value = v as f32;
    }
    changed
}

/// Slider with a colored track (e.g. temperature blue → yellow).
pub fn slider_gradient(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f64,
    range: std::ops::RangeInclusive<f64>,
    default: f64,
    step: f64,
    hint: &str,
    grad: [Color32; 2],
) -> bool {
    slider_full(ui, label, value, range, default, step, hint, Some(grad), 0.0)
}

/// Range slider for a hue range:
/// track with gradient `color_at(0 … 100)`, two knobs for from/to (values 0 … 100,
/// distance at least `min_span`). The area outside is darkened. Dragging moves the
/// nearer knob, double-click on name or track resets to `default`.
#[allow(clippy::too_many_arguments)]
pub fn hue_range(ui: &mut egui::Ui, label: &str, value: &mut [f32; 2], default: [f32; 2], min_span: f32, color_at: impl Fn(f32) -> Color32, hint: &str) -> bool {
    let before = *value;
    let enabled = ui.is_enabled();
    // a little air between the sliders (the row is taller, the slider itself sits at the top)
    let (outer, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 31.0 + SLIDER_AIR), egui::Sense::hover());
    let row = egui::Rect::from_min_size(outer.min + egui::vec2(0.0, SLIDER_AIR / 2.0), egui::vec2(outer.width(), 31.0));
    let pad = 7.0;
    let label_rect = egui::Rect::from_min_size(row.min, egui::vec2(row.width() - 64.0, 17.0));
    let value_rect = egui::Rect::from_min_max(egui::pos2(row.right() - 60.0, row.top()), egui::pos2(row.right(), row.top() + 17.0));
    let track_rect = egui::Rect::from_min_max(egui::pos2(row.left() + pad, row.top() + 17.0), egui::pos2(row.right() - pad, row.bottom()));
    let id = ui.scope_id().with(("lr-hue-range", label));
    let name = ui.interact(label_rect, id.with("name"), egui::Sense::click());
    let track = ui.interact(track_rect.expand2(egui::vec2(pad, 0.0)), id.with("track"), egui::Sense::click_and_drag());
    let x_of = |v: f32| track_rect.left() + track_rect.width() * (v / 100.0).clamp(0.0, 1.0);
    let v_of = |x: f32| (100.0 * (x - track_rect.left()) / track_rect.width()).clamp(0.0, 100.0).round();
    let set = |value: &mut [f32; 2], which: usize, v: f32| {
        if which == 0 {
            value[0] = v.min(value[1] - min_span).max(0.0);
        } else {
            value[1] = v.max(value[0] + min_span).min(100.0);
        }
    };
    if enabled {
        if name.double_clicked() || track.double_clicked() {
            crate::history::reset_gesture();
            *value = default;
        } else if track.drag_started() || track.clicked() {
            if let Some(p) = track.interact_pointer_pos() {
                let v = v_of(p.x);
                let which = usize::from((v - value[0]).abs() > (v - value[1]).abs() || (v > value[1]));
                ui.data_mut(|d| d.insert_temp(id, which));
                set(value, which, v);
            }
        } else if track.dragged() {
            if let Some(p) = track.interact_pointer_pos() {
                let which: usize = ui.data(|d| d.get_temp(id)).unwrap_or(0);
                set(value, which, v_of(p.x));
            }
        }
    }
    let hot = enabled && (ui.rect_contains_pointer(row) || track.dragged());
    let painter = ui.painter();
    let changed = *value != default;
    let name_color = if !enabled {
        Color32::from_gray(0x5c)
    } else if hot {
        Color32::WHITE
    } else if changed {
        Color32::from_gray(0xe6)
    } else {
        SLIDER_LABEL
    };
    let name_text = label_caps(painter, label_rect.left_center(), label, name_color);
    let text = format!("{:.0} / {:.0}", value[0], value[1]);
    painter.text(value_rect.right_center(), egui::Align2::RIGHT_CENTER, text, egui::TextStyle::Body.resolve(ui.style()), if changed && enabled { Color32::WHITE } else { TEXT_DIM });

    // Track with gradient, darkened outside the range
    let y = track_rect.center().y;
    let rail = egui::Rect::from_min_max(egui::pos2(track_rect.left(), y - 2.0), egui::pos2(track_rect.right(), y + 2.0));
    let alpha = if enabled { 1.0 } else { 0.35 };
    let mut mesh = egui::Mesh::default();
    const STEPS: usize = 24;
    for i in 0..=STEPS {
        let t = i as f32 / STEPS as f32;
        let c = color_at(t * 100.0).gamma_multiply(alpha);
        let x = rail.left() + rail.width() * t;
        mesh.colored_vertex(egui::pos2(x, rail.top()), c);
        mesh.colored_vertex(egui::pos2(x, rail.bottom()), c);
        if i > 0 {
            let k = (2 * i) as u32;
            mesh.add_triangle(k - 2, k - 1, k);
            mesh.add_triangle(k - 1, k, k + 1);
        }
    }
    painter.add(egui::Shape::mesh(mesh));
    let (xa, xb) = (x_of(value[0]), x_of(value[1]));
    let shade = Color32::from_black_alpha(150);
    painter.rect_filled(egui::Rect::from_min_max(rail.left_top(), egui::pos2(xa, rail.bottom())), 0.0, shade);
    painter.rect_filled(egui::Rect::from_min_max(egui::pos2(xb, rail.top()), rail.right_bottom()), 0.0, shade);
    let which: Option<usize> = track.dragged().then(|| ui.data(|d| d.get_temp(id)).unwrap_or(0));
    for (i, x) in [xa, xb].into_iter().enumerate() {
        let active = which == Some(i);
        let r = if active { 7.0 } else if enabled && track.hovered() { 6.5 } else { 5.5 };
        let center = egui::pos2(x, y);
        if active {
            painter.circle_filled(center, 12.0, Color32::from_white_alpha(28));
        }
        painter.circle_filled(center + egui::vec2(0.0, 1.0), r + 1.0, Color32::from_black_alpha(110));
        painter.circle_filled(center, r, if !enabled { Color32::from_gray(0x66) } else if hot { Color32::WHITE } else { Color32::from_gray(0xcf) });
    }
    if enabled && (track.hovered() || track.dragged()) {
        ui.ctx().set_cursor_icon(if track.dragged() { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab });
    }
    name_hover(ui, name, name_text, id, hot, label, hint, "Knöpfe ziehen: Bereich von/bis · Doppelklick: zurücksetzen");
    *value != before
}

/// Slider in the style of common photo apps: name (left) and value (right) on top,
/// below it the track across the full width with a round knob.
#[allow(clippy::too_many_arguments)]
fn slider_full(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f64,
    range: std::ops::RangeInclusive<f64>,
    default: f64,
    step: f64,
    hint: &str,
    grad: Option<[Color32; 2]>,
    inset: f32,
) -> bool {
    let before = *value;
    let (lo, hi) = (*range.start(), *range.end());
    let enabled = ui.is_enabled();
    // a little air between the sliders (the row is taller, the slider itself sits in the middle)
    let (outer, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 31.0 + SLIDER_AIR), egui::Sense::hover());
    let row = egui::Rect::from_min_size(outer.min + egui::vec2(0.0, SLIDER_AIR / 2.0), egui::vec2(outer.width(), 31.0));
    let pad = 7.0;
    // the name may start right of the row (`inset`, text edge of a tool tray); track and value keep their place
    let label_rect = egui::Rect::from_min_size(row.min + egui::vec2(inset, 0.0), egui::vec2(row.width() - 64.0 - inset, 17.0));
    let value_rect = egui::Rect::from_min_max(egui::pos2(row.right() - 60.0, row.top()), egui::pos2(row.right(), row.top() + 17.0));
    let track_rect = egui::Rect::from_min_max(egui::pos2(row.left() + pad, row.top() + 17.0), egui::pos2(row.right() - pad, row.bottom()));
    let id = ui.scope_id().with(("lr-slider", label));
    let name = ui.interact(label_rect, id.with("name"), egui::Sense::click());
    let track = ui.interact(track_rect.expand2(egui::vec2(pad, 0.0)), id.with("track"), egui::Sense::click_and_drag());
    let x_of = |v: f64| track_rect.left() + track_rect.width() * ((v - lo) / (hi - lo)).clamp(0.0, 1.0) as f32;
    let v_of = |x: f32| lo + (hi - lo) * ((x - track_rect.left()) / track_rect.width()).clamp(0.0, 1.0) as f64;
    let quantize = |v: f64, q: f64| ((v / q).round() * q).clamp(lo, hi);
    let alt = ui.input(|i| i.modifiers.alt);
    // Only real operation counts as a change (double-click, click, drag, typing, arrow keys). A value outside
    // `range` (e.g. an as-shot temperature of 50000 K on a 2000…12000 K slider) is shown at the end of the track
    // and kept as it is: drawing never changes it.
    let mut operated = false;

    if enabled {
        if name.double_clicked() || track.double_clicked() {
            crate::history::reset_gesture();
            *value = default;
            operated = true;
        } else if track.drag_started() || track.clicked() {
            operated = true;
            // Click beside the knob: jump there; on the knob: drag relatively
            if let Some(p) = track.interact_pointer_pos() {
                if (p.x - x_of(*value)).abs() > 9.0 {
                    *value = quantize(v_of(p.x), step);
                }
            }
            ui.data_mut(|d| d.insert_temp(id, *value));
        } else if track.dragged() {
            operated = true;
            let mut raw: f64 = ui.data(|d| d.get_temp(id)).unwrap_or(*value);
            let k = if alt { 0.1 } else { 1.0 };
            raw = (raw + track.drag_delta().x as f64 * (hi - lo) / track_rect.width() as f64 * k).clamp(lo, hi);
            ui.data_mut(|d| d.insert_temp(id, raw));
            *value = quantize(raw, if alt { step * 0.1 } else { step });
        }
    }

    // layer-aware: if a dropdown lies above it, the slider doesn't count as hovered
    let hot = enabled && (ui.rect_contains_pointer(row) || track.dragged());
    let active = enabled && track.dragged();
    let knob_hot = enabled && (track.hovered() || active);
    let painter = ui.painter();
    let changed = (*value - default).abs() > step * 0.05;

    // Name: double-click resets; changed sliders slightly brighter
    let name_color = if !enabled {
        Color32::from_gray(0x5c)
    } else if hot {
        Color32::WHITE
    } else if changed {
        Color32::from_gray(0xe6)
    } else {
        SLIDER_LABEL
    };
    let name_text = label_caps(painter, label_rect.left_center(), label, name_color);

    // Track: hairline with a fine tick scale (every 5 %), centre mark on two-sided sliders,
    // optionally as a gradient; the fill runs from the default value to the value.
    let y = track_rect.center().y;
    let rail = egui::Rect::from_min_max(egui::pos2(track_rect.left(), y - 0.5), egui::pos2(track_rect.right(), y + 0.5));
    let dim = if enabled { 1.0 } else { 0.55 };
    for i in 0..=20 {
        let tx = track_rect.left() + track_rect.width() * i as f32 / 20.0;
        let major = i % 5 == 0;
        let (t0, t1) = if major { (y + 3.0, y + 6.0) } else { (y + 3.0, y + 4.5) };
        let c = Color32::from_gray(if major { 0x66 } else { 0x4a }).gamma_multiply(dim);
        painter.line_segment([egui::pos2(tx, t0), egui::pos2(tx, t1)], Stroke::new(1.0, c));
    }
    match grad {
        Some([a, b]) => {
            let rail = rail.expand2(egui::vec2(0.0, 0.75));
            let mut mesh = egui::Mesh::default();
            let (ca, cb) = (a.gamma_multiply(dim), b.gamma_multiply(dim));
            mesh.colored_vertex(rail.left_top(), ca);
            mesh.colored_vertex(rail.right_top(), cb);
            mesh.colored_vertex(rail.right_bottom(), cb);
            mesh.colored_vertex(rail.left_bottom(), ca);
            mesh.add_triangle(0, 1, 2);
            mesh.add_triangle(0, 2, 3);
            painter.add(egui::Shape::mesh(mesh));
        }
        None => {
            painter.rect_filled(rail, 0.0, if hot { Color32::from_gray(0x5a) } else { Color32::from_gray(0x46) });
        }
    }
    let x0 = x_of(default);
    let x = x_of(*value);
    if changed && grad.is_none() {
        let fill = if enabled { if hot { Color32::from_gray(0xe0) } else { Color32::from_gray(0xb0) } } else { Color32::from_gray(0x5c) };
        painter.rect_filled(egui::Rect::from_min_max(egui::pos2(x0.min(x), y - 1.0), egui::pos2(x0.max(x), y + 1.0)), 0.0, fill);
    }
    // Centre mark on two-sided sliders
    if lo < default && default < hi {
        painter.line_segment([egui::pos2(x0, y - 4.5), egui::pos2(x0, y + 6.0)], Stroke::new(1.0, Color32::from_gray(0x80).gamma_multiply(dim)));
    }
    // Knob: narrow vertical line, wider on hover, with a halo while dragging
    let (kw, kh) = if active { (3.0, 15.0) } else if knob_hot { (3.0, 14.0) } else { (2.0, 12.0) };
    let center = egui::pos2(x, y);
    if active {
        painter.rect_filled(egui::Rect::from_center_size(center, egui::vec2(11.0, 19.0)), 0.0, Color32::from_white_alpha(22));
    }
    painter.rect_filled(egui::Rect::from_center_size(center + egui::vec2(0.0, 0.5), egui::vec2(kw + 2.0, kh + 2.0)), 0.0, Color32::from_black_alpha(140));
    let knob = if !enabled {
        Color32::from_gray(0x66)
    } else if knob_hot {
        Color32::WHITE
    } else {
        Color32::from_gray(0xd8)
    };
    painter.rect_filled(egui::Rect::from_center_size(center, egui::vec2(kw, kh)), 0.0, knob);
    if knob_hot {
        ui.ctx().set_cursor_icon(if active { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab });
    }

    // Value: frameless, shown as a field on hover; sign on two-sided sliders
    let dec = decimals(step);
    let signed = lo < 0.0;
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(value_rect).layout(egui::Layout::right_to_left(egui::Align::Center)));
    {
        let v = child.visuals_mut();
        v.override_text_color = Some(if changed && enabled { Color32::WHITE } else { TEXT_DIM });
        v.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
        v.widgets.inactive.bg_fill = Color32::TRANSPARENT;
        v.widgets.inactive.bg_stroke = Stroke::NONE;
    }
    let field = child.add_sized(
        value_rect.size(),
        egui::DragValue::new(value)
            .range(range.clone())
            // keep values outside the range as they are while drawing (egui clamps them by default and reports a
            // change); typed or dragged values are still clamped
            .clamp_existing_to_range(false)
            .speed(step)
            .custom_formatter(move |v, _| {
                if signed && v.abs() >= 0.5 * 10f64.powi(-(dec as i32)) {
                    format!("{v:+.dec$}")
                } else {
                    format!("{:.dec$}", v.abs())
                }
            }),
    );
    operated |= field.changed();
    name_hover(ui, name, name_text, id, hot, label, hint, SLIDER_CONTROLS);

    if enabled && hot {
        let (up, down, shift) = ui.input(|i| (i.key_pressed(egui::Key::ArrowUp), i.key_pressed(egui::Key::ArrowDown), i.modifiers.shift));
        let k = if shift { 10.0 } else if alt { 0.1 } else { 1.0 };
        if up {
            *value = (*value + step * k).clamp(lo, hi);
            operated = true;
        }
        if down {
            *value = (*value - step * k).clamp(lo, hi);
            operated = true;
        }
    }
    operated && *value != before
}

/// Operation of a slider (shown in the "?" card of the slider).
const SLIDER_CONTROLS: &str = "Klicken zum Eintippen · waagerecht ziehen zum Verstellen · Doppelklick: zurücksetzen · ⌥ beim Ziehen: fein · ↑/↓ über dem Regler";

/// Operation of the header of an edit section (shown in its "?" card): switch, reset, group headings, context menu.
const SECTION_CONTROLS: &str = "Schalter im Kopf: Abschnitt vorübergehend abschalten (Werte bleiben), wieder einschalten · ↺: Abschnitt auf Standard zurücksetzen · Gruppenüberschrift: Doppelklick oder ⌥-Klick setzt die Gruppe zurück · Rechtsklick auf den Kopf: kopieren, einfügen, zurücksetzen";

/// Help for the slider name: if there is a help card ([`crate::help`]), it appears when
/// hovering over the name, or immediately over the "?", which only shows up right behind the name
/// while the row is hovered; otherwise the previous short hint as a tooltip.
#[allow(clippy::too_many_arguments)]
fn name_hover(ui: &egui::Ui, name: egui::Response, name_text: egui::Rect, id: egui::Id, hot: bool, label: &str, hint: &str, controls: &str) {
    if hint == PLAIN {
        return;
    }
    match crate::help::for_slider(label, hint) {
        Some(card) => {
            let q = egui::pos2(name_text.right() + 10.0, name_text.center().y);
            crate::help::question_mark(ui, q, id.with("hilfe"), hot, label, hint, card, controls);
            let _ = name;
        }
        None => {
            // without a help card: the hint as a tooltip on the name, only when it is short (one line)
            if hint.chars().count() <= 70 {
                hover_tip(ui, &name, hint);
            }
        }
    }
}

/// Group heading inside a section (e.g. "Tone"). With ⌥ held down
/// it becomes "Reset Tone"; double-click resets as well.
pub fn group_label(ui: &mut egui::Ui, text: &str) -> bool {
    remember_section(text);
    let alt = ui.input(|i| i.modifiers.alt);
    let shown = if alt { crate::trf!("{text} zurücksetzen", text = crate::tr(text)) } else { text.to_string() };
    let color = if alt { ACCENT } else { Color32::from_gray(0xbc) };
    let g = caps_spaced(ui.painter(), &shown, egui::FontId::proportional(10.5), color, 1.0);
    let (rect, r) = ui.allocate_exact_size(egui::vec2(g.size().x, 16.0), egui::Sense::click());
    ui.painter().galley(egui::pos2(rect.left(), rect.center().y - g.size().y / 2.0), g, color);
    // in lists without sections (film shelf of the darkroom) the group heading sticks to the top
    let owned = text.to_string();
    let band = egui::Rect::from_min_max(egui::pos2(rect.left(), rect.top() - 4.0), egui::pos2(rect.left() + ui.available_width().max(rect.width()), rect.bottom() + 4.0));
    crate::sticky::register(crate::sticky::RANK_CATEGORY, ui.id().with(("gruppe", text)), band, move |p, bar, _| {
        let g = caps_spaced(p, &owned, egui::FontId::proportional(10.5), Color32::from_gray(0xbc), 1.0);
        p.galley(egui::pos2(bar.left(), bar.center().y - g.size().y / 2.0), g, Color32::from_gray(0xbc));
    });
    r.double_clicked() || (alt && r.clicked())
}

fn decimals(step: f64) -> usize {
    if step >= 1.0 {
        0
    } else if step >= 0.1 {
        1
    } else {
        2
    }
}

/// Toggle made of adjoining segments (e.g. Color | Black & White): shared frame,
/// the active segment has a light fill. Returns: was it toggled?
pub fn segmented<T: PartialEq + Clone>(ui: &mut egui::Ui, value: &mut T, options: &[(T, &str)]) -> bool {
    segmented_pad(ui, value, options, 10.0)
}

/// Like [`segmented`] with a selectable inner padding per segment (narrow for rows with many values,
/// so the sidebar doesn't grow wider than its space).
pub fn segmented_pad<T: PartialEq + Clone>(ui: &mut egui::Ui, value: &mut T, options: &[(T, &str)], pad: f32) -> bool {
    segmented_tips(ui, value, options, &[], pad)
}

/// Like [`segmented_pad`] with a tooltip per segment (`tips[i]`, German text, empty = none).
pub fn segmented_tips<T: PartialEq + Clone>(ui: &mut egui::Ui, value: &mut T, options: &[(T, &str)], tips: &[&str], pad: f32) -> bool {
    segmented_fill(ui, value, options, tips, pad, ACTIVE_FILL)
}

/// Like [`segmented`], the chosen segment filled in [`BLUE`]: for a choice that decides what happens (import mode,
/// kind of source), so the setting is visible at a glance.
pub fn segmented_blue<T: PartialEq + Clone>(ui: &mut egui::Ui, value: &mut T, options: &[(T, &str)]) -> bool {
    segmented_fill(ui, value, options, &[], 10.0, BLUE)
}

fn segmented_fill<T: PartialEq + Clone>(ui: &mut egui::Ui, value: &mut T, options: &[(T, &str)], tips: &[&str], pad: f32, active_fill: Color32) -> bool {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let h = 20.0;
    let widths: Vec<f32> = options
        .iter()
        .map(|(_, l)| ui.painter().layout_no_wrap(crate::tr(l).to_string(), font.clone(), TEXT).size().x + 2.0 * pad)
        .collect();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(widths.iter().sum(), h), egui::Sense::hover());
    let painter = ui.painter().clone();
    painter.rect_filled(rect, 0.0, BUTTON_FILL);
    let base = ui.scope_id().with(options.first().map(|o| o.1).unwrap_or_default());
    let mut x = rect.left();
    let mut changed = false;
    for (i, ((v, label), w)) in options.iter().zip(&widths).enumerate() {
        let seg = egui::Rect::from_min_size(egui::pos2(x, rect.top()), egui::vec2(*w, h));
        x += w;
        let resp = ui.interact(seg, base.with(i), egui::Sense::click());
        if resp.clicked() && *value != *v {
            *value = v.clone();
            changed = true;
        }
        let active = *value == *v;
        if active {
            painter.rect_filled(seg, 0.0, if ui.is_enabled() { active_fill } else { active_fill.gamma_multiply(0.5) });
        } else if resp.hovered() {
            painter.rect_filled(seg, 0.0, Color32::from_gray(0x33));
        }
        if let Some(tip) = tips.get(i).filter(|t| !t.is_empty()) {
            hover_tip(ui, &resp, crate::tr(tip));
        }
        let color = if active { Color32::WHITE } else if resp.hovered() { TEXT } else { TEXT_DIM };
        let galley = painter.layout_no_wrap(crate::tr(label).to_string(), font.clone(), color);
        painter.galley(seg.center() - galley.size() / 2.0, galley, color);
    }
    changed
}

/// Content of a tool tray without its own section header (the header of the right panel already names it).
/// Returns as for the sections: reset clicked (never here).
pub fn panel(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) -> bool {
    add(ui);
    false
}

/// Primary button: the single most likely action of a window or area (Done, Import,
/// Capture …) – filled gold with dark text. Only one per window; destructive actions stay red,
/// secondary actions stay grey. `active = false`: greyed out, not clickable.
pub fn main_button(ui: &mut egui::Ui, text: &str, active: bool) -> egui::Response {
    main_button_min(ui, text, active, 0.0)
}

/// Like [`main_button`] with a minimum width (equally wide buttons in dialogs).
pub fn main_button_min(ui: &mut egui::Ui, text: &str, active: bool, min_width: f32) -> egui::Response {
    let text = crate::tr(text);
    let font = egui::TextStyle::Button.resolve(ui.style());
    let dark = Color32::from_rgb(0x16, 0x14, 0x10);
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, dark);
    let size = egui::vec2((galley.size().x + 24.0).max(min_width), 24.0);
    let (rect, resp) = ui.allocate_exact_size(size, if active { egui::Sense::click() } else { egui::Sense::hover() });
    let (fill, fg) = if !active {
        (Color32::from_gray(0x2a), Color32::from_gray(0x66))
    } else if resp.is_pointer_button_down_on() {
        (Color32::from_rgb(0xa8, 0x8e, 0x50), dark)
    } else if resp.hovered() {
        (Color32::from_rgb(0xdc, 0xbf, 0x76), dark)
    } else {
        (GOLD, dark)
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, fill);
    if resp.has_focus() {
        painter.rect_stroke(rect, 0.0, Stroke::new(1.5, Color32::WHITE), egui::StrokeKind::Inside);
    }
    let g = painter.layout_no_wrap(text.to_string(), egui::TextStyle::Button.resolve(ui.style()), fg);
    painter.galley(rect.center() - g.size() / 2.0, g, fg);
    resp
}

/// Button of the tool rail on the right edge: icon 36 × 34, light fill when active, no frame.
/// `available = false`: greyed out. Returns: clicked.
pub fn tool_rail_button(ui: &mut egui::Ui, icon: Icon, on: bool, available: bool, tip: &str) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(36.0, 34.0), if available { egui::Sense::click() } else { egui::Sense::hover() });
    let (fill, fg) = if on {
        (Color32::from_gray(0x3a), Color32::WHITE)
    } else if resp.hovered() && available {
        (Color32::from_gray(0x26), Color32::from_gray(0xe8))
    } else {
        (Color32::TRANSPARENT, if available { Color32::from_gray(0xa4) } else { Color32::from_gray(0x56) })
    };
    let painter = ui.painter();
    painter.rect_filled(rect.shrink2(egui::vec2(2.0, 1.0)), 0.0, fill);
    paint_icon(painter, egui::Rect::from_center_size(rect.center(), egui::vec2(20.0, 20.0)), icon, fg);
    if resp.has_focus() {
        painter.rect_stroke(rect.shrink2(egui::vec2(2.0, 1.0)), 0.0, Stroke::new(1.5, ACCENT), egui::StrokeKind::Inside);
    }
    hover_tip(ui, &resp, tip);
    resp.clicked() && available
}

/// Category header in the edit panel ("Light & Color", "Details" …): a band across the width,
/// followed by the sections of the category.
pub fn category(ui: &mut egui::Ui, text: &str) {
    // Separator instead of a box: small dimmed title in capitals, a thin line to the edge on the right
    ui.add_space(14.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 18.0), egui::Sense::hover());
    category_paint(ui.painter(), rect, text);
    // sticks to the top of a long list while its content scrolls (a little air above and below the line)
    let owned = text.to_string();
    crate::sticky::register(crate::sticky::RANK_CATEGORY, ui.id().with(("kategorie", text)), rect.expand2(egui::vec2(0.0, 5.0)), move |p, bar, _| {
        category_paint(p, egui::Rect::from_center_size(bar.center(), egui::vec2(bar.width(), 18.0)), &owned);
    });
    ui.add_space(2.0);
}

/// [`category`] with controls at its right end (e.g. "?" or a toggle), `right` points wide; the line ends before them.
pub fn category_with<R>(ui: &mut egui::Ui, text: &str, right: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.add_space(12.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 22.0), egui::Sense::hover());
    let line = rect.with_max_x((rect.right() - right - 6.0).max(rect.left()));
    category_paint(ui.painter(), egui::Rect::from_center_size(line.center(), egui::vec2(line.width(), 18.0)), text);
    let owned = text.to_string();
    crate::sticky::register(crate::sticky::RANK_CATEGORY, ui.scope_id().with(("kategorie", text)), rect.expand2(egui::vec2(0.0, 3.0)), move |p, bar, _| {
        category_paint(p, egui::Rect::from_center_size(bar.center(), egui::vec2(bar.width(), 18.0)), &owned);
    });
    let area = egui::Rect::from_min_max(egui::pos2((rect.right() - right).max(rect.left()), rect.top()), rect.max);
    let mut child = ui.new_child(egui::UiBuilder::new().id_salt(("category_with", text)).max_rect(area).layout(egui::Layout::right_to_left(egui::Align::Center)));
    let r = add(&mut child);
    ui.add_space(2.0);
    r
}

fn category_paint(painter: &egui::Painter, rect: egui::Rect, text: &str) {
    let g = caps(painter, text, egui::FontId::proportional(10.5), CATEGORY_LABEL);
    let links = rect.left() + TEXT_INSET;
    painter.galley(egui::pos2(links, rect.center().y - g.size().y / 2.0), g.clone(), CATEGORY_LABEL);
    let x = links + g.size().x + 10.0;
    if x < rect.right() {
        painter.line_segment([egui::pos2(x, rect.center().y), egui::pos2(rect.right(), rect.center().y)], egui::Stroke::new(1.0, Color32::from_gray(0x3a)));
    }
}

/// Toggle button with frame; light fill when on.
pub fn toggle(ui: &mut egui::Ui, on: &mut bool, label: &str) -> egui::Response {
    let r = ui.add(egui::Button::new(crate::tr(label)).selected(*on));
    if r.clicked() {
        *on = !*on;
    }
    r
}

/// Keep the beginning and end of a path or filename visible within a measured width.
fn middle_ellipsis(ui: &egui::Ui, text: &str, width: f32) -> String {
    middle_ellipsis_font(ui.painter(), text, &egui::TextStyle::Body.resolve(ui.style()), width)
}

/// [`middle_ellipsis`] for any font (e.g. a bold folder name or a small path line).
pub fn middle_ellipsis_font(painter: &egui::Painter, text: &str, font: &egui::FontId, width: f32) -> String {
    let measure = |s: &str| painter.layout_no_wrap(s.to_owned(), font.clone(), TEXT).size().x;
    if measure(text) <= width { return text.to_owned(); }
    let chars: Vec<char> = text.chars().collect();
    let mut lo = 0;
    let mut hi = chars.len();
    while lo < hi {
        let n = (lo + hi + 1) / 2;
        let left = n.div_ceil(2);
        let right = n / 2;
        let candidate = format!("{}…{}", chars[..left].iter().collect::<String>(), chars[chars.len() - right..].iter().collect::<String>());
        if measure(&candidate) <= width { lo = n; } else { hi = n - 1; }
    }
    let left = lo.div_ceil(2);
    let right = lo / 2;
    format!("{}…{}", chars[..left].iter().collect::<String>(), chars[chars.len() - right..].iter().collect::<String>())
}

/// A chosen value (folder, target, catalog) in its own field: slightly lighter ground, one line, paths shortened in
/// the middle, the full text as tooltip. `None`: the empty state, dimmed (e.g. "Noch nichts gewählt").
pub fn value_field(ui: &mut egui::Ui, value: Option<&str>, empty: &str) -> egui::Response {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 22.0), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 0.0, Color32::from_gray(0x26));
    p.rect_stroke(rect, 0.0, egui::Stroke::new(1.0, FIELD_STROKE), egui::StrokeKind::Inside);
    let font = egui::FontId::proportional(UI_SIZE - 0.5);
    let (text, color) = match value {
        Some(v) => (middle_ellipsis_font(p, v, &font, rect.width() - 14.0), Color32::from_gray(0xe6)),
        None => (crate::tr(empty).to_string(), Color32::from_gray(0x80)),
    };
    let g = p.layout_no_wrap(text, font, color);
    p.galley(egui::pos2(rect.left() + 7.0, rect.center().y - g.size().y / 2.0), g, color);
    if let Some(v) = value {
        hover_tip(ui, &resp, v);
    }
    resp
}

/// Start of a section in a dialog column: more air above, a thin rule, then the content (headings follow).
pub fn section_rule(ui: &mut egui::Ui) {
    ui.add_space(8.0);
    let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().hline(r.x_range(), r.center().y, egui::Stroke::new(1.0, Color32::from_gray(0x2c)));
    ui.add_space(4.0);
}

/// Does `text` fit into `width` on one line (body font)?
pub fn fits_width(ui: &egui::Ui, text: &str, width: f32) -> bool {
    let font = egui::TextStyle::Body.resolve(ui.style());
    ui.painter().layout_no_wrap(text.to_owned(), font, TEXT).size().x <= width
}

/// `text` shortened at the start ("…/Fotos/2024/Rom") so that it fits into `width`: for paths, whose end says most.
fn start_ellipsis(ui: &egui::Ui, text: &str, width: f32) -> String {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let measure = |s: &str| ui.painter().layout_no_wrap(s.to_owned(), font.clone(), TEXT).size().x;
    if measure(text) <= width {
        return text.to_owned();
    }
    let chars: Vec<char> = text.chars().collect();
    let (mut lo, mut hi) = (0, chars.len());
    while lo < hi {
        let n = (lo + hi + 1) / 2;
        if measure(&format!("…{}", chars[chars.len() - n..].iter().collect::<String>())) <= width { lo = n; } else { hi = n - 1; }
    }
    format!("…{}", chars[chars.len() - lo..].iter().collect::<String>())
}

/// Gap between the label column and the value of a [`kv_row`].
const KV_GAP: f32 = 10.0;
/// The label column of [`kv_row`] takes at most this share of the width.
const KV_KEY_SHARE: f32 = 0.4;

/// Width of the label column of the rows in `ui`: the longest label of the group – remembered from
/// the last frame, together with the labels of this frame so far – at most [`KV_KEY_SHARE`] of the width.
fn kv_key_column(ui: &egui::Ui, key_w: f32, width: f32) -> f32 {
    let id = ui.scope_id().with("kv-key-column");
    let pass = ui.ctx().cumulative_pass_nr();
    let (stored, cur, prev): (u64, f32, f32) = ui.data(|d| d.get_temp(id)).unwrap_or((pass, 0.0, 0.0));
    let (cur, prev) = if stored == pass { (cur.max(key_w), prev) } else { (key_w, cur) };
    ui.data_mut(|d| d.insert_temp(id, (pass, cur, prev)));
    cur.max(prev).min(width * KV_KEY_SHARE).ceil()
}

/// A metadata row: the label right-aligned and dimmed in a column as wide as the longest label of the
/// group, the value left-aligned after a fixed gap – one clear axis. Paths are shortened at the start, other long
/// values in the middle; values made of parts ("1/50 s · f/2,8 · ISO 400") wrap at their separators instead. The full
/// text as tooltip only where something is cut, plus "Kopieren" in the context menu.
/// Returns the value response, so callers can attach an action to it.
pub fn kv_row(ui: &mut egui::Ui, key: &str, value: &str, clickable: bool) -> egui::Response {
    kv_row_tip(ui, key, value, clickable, "")
}

/// [`kv_row`] with a tooltip for a clickable value (a shortened value names itself first). A clickable value is
/// highlighted while the pointer is over it (brighter, underlined; the arrow pointer stays).
pub fn kv_row_tip(ui: &mut egui::Ui, key: &str, value: &str, clickable: bool, tip: &str) -> egui::Response {
    // never past the visible edge of the panel (its clip can end before the available width)
    let width = ui.available_width().min(ui.clip_rect().right() - ui.cursor().left()).max(0.0);
    let font = egui::TextStyle::Body.resolve(ui.style());
    let key_w = ui.painter().layout_no_wrap(key.to_owned(), font.clone(), TEXT_DIM).size().x;
    let key_col = kv_key_column(ui, key_w, width);
    let value_width = (width - key_col - KV_GAP).max(0.0);
    let path = value.contains('/') && !value.contains(" · ") || value.contains('\\');
    let file_name = clickable || key == crate::tr("Dateiname");
    let fits = fits_width(ui, value, value_width);
    let wrap = !fits && !path && !file_name && value.contains(" · ");
    let display = if fits || wrap {
        value.to_owned()
    } else if path {
        start_ellipsis(ui, value, value_width - 4.0)
    } else {
        middle_ellipsis(ui, value, value_width - 4.0)
    };
    // a value in parts wraps after a separator (" ·" kept at the line end), never inside a part: as many parts per
    // line as fit
    let shown = if wrap {
        let mut lines: Vec<String> = vec![String::new()];
        for part in display.split(" · ") {
            let last = lines.last_mut().expect("one line");
            let candidate = if last.is_empty() { part.to_string() } else { format!("{last} · {part}") };
            if last.is_empty() || fits_width(ui, &candidate, value_width) {
                *last = candidate;
            } else {
                last.push_str(" ·");
                lines.push(part.to_string());
            }
        }
        lines.join("\n")
    } else {
        display.clone()
    };
    let lines = if wrap { ui.painter().layout(shown.clone(), font.clone(), TEXT, value_width).rows.len().max(1) } else { 1 };
    let row_h = 20.0 + (lines as f32 - 1.0) * (font.size + 4.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, row_h), egui::Sense::hover());
    let key_rect = egui::Rect::from_min_size(rect.min, egui::vec2(key_col, 20.0));
    let value_rect = egui::Rect::from_min_max(egui::pos2(rect.left() + key_col + KV_GAP, rect.top()), rect.max);
    let key_response = ui
        .scope_builder(egui::UiBuilder::new().max_rect(key_rect).layout(egui::Layout::right_to_left(egui::Align::Center)), |ui| {
            ui.add(egui::Label::new(egui::RichText::new(key).color(TEXT_DIM)).truncate().selectable(false))
        })
        .inner;
    if key_w > key_col + 0.5 {
        hover_tip(ui, &key_response, key);
    }
    let shortened = display != value;
    let response = ui
        .scope_builder(egui::UiBuilder::new().max_rect(value_rect).layout(egui::Layout::left_to_right(egui::Align::Min)), |ui| {
            ui.set_min_height(20.0);
            let hot = clickable && ui.rect_contains_pointer(value_rect);
            let text = egui::RichText::new(shown).color(if hot { Color32::WHITE } else { TEXT });
            let text = if hot { text.underline() } else { text };
            let label = egui::Label::new(text).selectable(!clickable);
            let label = if wrap { label.wrap() } else { label.truncate() };
            let label = if clickable { label.sense(egui::Sense::click()) } else { label };
            // one line: centred in the 20 pt row like the label
            ui.add_space(0.0);
            ui.allocate_ui_with_layout(egui::vec2(value_width, row_h), egui::Layout::left_to_right(if wrap { egui::Align::Min } else { egui::Align::Center }), |ui| ui.add(label)).inner
        })
        .inner;
    match (shortened, tip.is_empty()) {
        (true, true) => hover_tip(ui, &response, value),
        (true, false) => hover_tip(ui, &response, &format!("{value}\n{tip}")),
        (false, false) => hover_tip(ui, &response, tip),
        (false, true) => {}
    }
    if shortened {
        response.context_menu(|ui| {
            if ui.button(crate::tr("Kopieren")).clicked() {
                ui.ctx().copy_text(value.to_owned());
                ui.close();
            }
        });
    }
    response
}

/// Animated on/off switch shared by dialogs and settings.
pub fn switch(ui: &mut egui::Ui, on: &mut bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(36.0, 20.0), egui::Sense::click());
    if response.clicked() { *on = !*on; }
    let t = ui.ctx().animate_bool(response.id, *on);
    let bg = Color32::from_gray(0x5c).lerp_to_gamma(BLUE, t);
    let bg = if ui.is_enabled() { bg } else { bg.gamma_multiply(0.5) };
    ui.painter().rect_filled(rect, 10.0, bg);
    let x = egui::lerp(rect.left() + 10.0..=rect.right() - 10.0, t);
    ui.painter().circle_filled(egui::pos2(x, rect.center().y), 8.0, if ui.is_enabled() { Color32::WHITE } else { Color32::from_gray(0x90) });
    response.clicked()
}

/// Label left of a control (same width as the slider names).
pub fn field_label(ui: &mut egui::Ui, text: &str) {
    // left-aligned like the slider names, fixed width for a clean column
    let (rect, _) = ui.allocate_exact_size(egui::vec2(96.0, 20.0), egui::Sense::hover());
    label_caps(ui.painter(), rect.left_center(), text, Color32::from_gray(0xa4));
}

/// Clickable text (link): underlined on hover in `color`, normal arrow pointer. Not selectable: egui shows the
/// text cursor (I-beam) over every selectable label, even a clickable one – that suggests typing instead of a click.
pub fn link_text(ui: &mut egui::Ui, text: egui::RichText, color: Color32) -> egui::Response {
    let r = ui.add(egui::Label::new(text.color(color)).selectable(false).sense(egui::Sense::click()));
    if r.hovered() {
        ui.painter().hline(r.rect.x_range(), r.rect.bottom() - 1.0, egui::Stroke::new(1.0, color));
    }
    r
}

/// List entry across the full width: highlighted on hover, pointer cursor, optionally
/// with a marker dot for the active entry.
pub fn list_item(ui: &mut egui::Ui, text: &str, active: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 20.0), egui::Sense::click());
    let painter = ui.painter();
    if active {
        painter.rect_filled(rect, 0.0, Color32::from_gray(0x4a));
    } else if resp.hovered() {
        painter.rect_filled(rect, 0.0, Color32::from_gray(0x38));
    }
    let color = if active || resp.hovered() { Color32::WHITE } else { TEXT };
    let galley = painter.layout_no_wrap(text.to_owned(), egui::TextStyle::Body.resolve(ui.style()), color);
    let text_rect = rect.shrink2(egui::vec2(8.0, 0.0));
    painter.with_clip_rect(text_rect).galley(egui::pos2(text_rect.left(), text_rect.center().y - galley.size().y / 2.0), galley.clone(), color);
    if galley.size().x > text_rect.width() {
        hover_tip(ui, &resp, text);
    }
    resp
}

/// Keep a vertical section inside its available width even if a child requests more space.
/// Only the child's height is transferred to the parent; painting and pointer input are clipped.
pub fn bounded<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let available = ui.available_rect_before_wrap();
    let width = available.width().max(0.0);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(available).layout(egui::Layout::top_down(egui::Align::Min)));
    // clip only the width: in a scroll area `available` is just the first screenful of the content, so clipping its
    // height cut off everything below it once the page was scrolled (settings page "Entwickeln")
    let clip = child.clip_rect();
    child.set_clip_rect(clip.intersect(egui::Rect::from_x_y_ranges(available.x_range(), clip.y_range())));
    let result = add(&mut child);
    let height = (child.min_rect().bottom() - available.top()).max(0.0);
    ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    result
}

/// Section header with a disclosure triangle; clicking the title expands/collapses.
/// Storage slots in the egui context: requested section action (title, 1 = copy, 2 = paste)
/// and title of the copied section (for "Paste").
pub const SECTION_ACTION: &str = "lr-section-action";
pub const SECTION_CLIP: &str = "lr-section-clip";

pub fn section<R>(ui: &mut egui::Ui, title: &str, open: bool, add: impl FnOnce(&mut egui::Ui) -> R) {
    section_reset(ui, title, open, false, add);
}

/// Like [`section`], but with a reset icon ↺ on the right of the header as soon as something in the section
/// differs from the default. Returns: reset?
/// Test aid: sections and groups drawn in this frame (for the test matrix of image types).
pub fn remember_section(t: &str) {
    if RECORD_SECTIONS.load(std::sync::atomic::Ordering::Relaxed) {
        RECORDED_SECTIONS.with(|a| a.borrow_mut().push(t.to_string()));
    }
}

/// Enable recording (only the test matrix; otherwise it costs nothing).
pub fn remember_sections_on() {
    RECORD_SECTIONS.store(true, std::sync::atomic::Ordering::Relaxed);
}

static RECORD_SECTIONS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Fetch the recorded sections (and clear them).
pub fn take_sections() -> Vec<String> {
    RECORDED_SECTIONS.with(|a| std::mem::take(&mut *a.borrow_mut()))
}

thread_local!(static RECORDED_SECTIONS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) });

pub fn section_reset<R>(ui: &mut egui::Ui, title: &str, open: bool, changed: bool, add: impl FnOnce(&mut egui::Ui) -> R) -> bool {
    remember_section(title);
    section_full(ui, title, open, changed, None, None, add)
}

/// [`section_reset`] for a tool's own sliders whose section titles also occur in "Edit" (e.g. the local corrections
/// "Licht", "Details"): own open/closed state under `salt`, no copy/paste menu and no help card of the Edit section.
pub fn section_local<R>(ui: &mut egui::Ui, salt: &str, title: &str, open: bool, changed: bool, add: impl FnOnce(&mut egui::Ui) -> R) -> bool {
    section_full(ui, title, open, changed, None, Some(salt), add)
}

/// Like [`section_reset`], additionally with a small toggle in the header: temporarily
/// disable the section without losing the sliders (`off` = disabled).
pub fn section_switch<R>(ui: &mut egui::Ui, title: &str, open: bool, changed: bool, off: &mut bool, add: impl FnOnce(&mut egui::Ui) -> R) -> bool {
    section_full(ui, title, open, changed, Some(off), None, add)
}

/// Arrow (rotates when expanding) and title of a section header; returns the title position and width.
/// Also used by the header that sticks to the top of a scrolling list (`sticky`), so both look alike.
fn section_head_paint(painter: &egui::Painter, rect: egui::Rect, title: &str, openness: f32, hovered: bool) -> (egui::Pos2, f32) {
    let c = egui::pos2(rect.left() + 10.0, rect.center().y);
    let a = openness * std::f32::consts::FRAC_PI_2;
    let rot = |p: egui::Vec2| c + egui::vec2(p.x * a.cos() - p.y * a.sin(), p.x * a.sin() + p.y * a.cos());
    let arrow = vec![rot(egui::vec2(-2.5, -4.0)), rot(egui::vec2(3.5, 0.0)), rot(egui::vec2(-2.5, 4.0))];
    let arrow_color = if hovered { Color32::from_gray(0xe8) } else { Color32::from_gray(0x9a) };
    painter.add(egui::Shape::convex_polygon(arrow, arrow_color, Stroke::NONE));
    let title_color = if hovered { Color32::WHITE } else { Color32::from_gray(0xe2) };
    let galley = caps(painter, title, semibold(UI_SIZE), title_color);
    let title_pos = egui::pos2(rect.left() + 22.0, rect.center().y - galley.size().y / 2.0);
    let title_w = galley.size().x.min((rect.right() - title_pos.x - 28.0).max(0.0));
    painter.with_clip_rect(rect.with_max_x((rect.right() - 28.0).max(rect.left()))).galley(title_pos, galley, title_color);
    (title_pos, title_w)
}

fn section_full<R>(ui: &mut egui::Ui, title: &str, open: bool, changed: bool, mut off: Option<&mut bool>, local: Option<&str>, add: impl FnOnce(&mut egui::Ui) -> R) -> bool {
    // Test hook (own data folder only): `GRAINEO_OPEN_SECTIONS="Aufnahmedetails,…"` opens these sections at first sight
    let open = open || crate::env::var_os("GRAINEO_DATA_DIR").is_some() && crate::env::var("GRAINEO_OPEN_SECTIONS").is_ok_and(|v| v.split(',').any(|t| t.trim() == title || local == Some(t.trim())));
    let id = match local {
        Some(salt) => egui::Id::unique(("lr-section-local", salt)),
        None => ui.make_persistent_id(("lr-section", title)),
    };
    let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, open);
    let mut reset = false;
    // Header row across the full width: click expands/collapses, hover highlights
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.0), egui::Sense::click());
    let reset_rect = egui::Rect::from_center_size(egui::pos2(rect.right() - 12.0, rect.center().y), egui::vec2(22.0, 22.0));
    let reset_resp = changed.then(|| ui.interact(reset_rect, id.with("reset"), egui::Sense::click()));
    // Toggle left of the reset icon (only when something is adjusted)
    let switch_rect = egui::Rect::from_center_size(egui::pos2(rect.right() - 40.0, rect.center().y), egui::vec2(24.0, 13.0));
    let switch_resp = (changed && off.is_some()).then(|| ui.interact(switch_rect.expand(4.0), id.with("switch"), egui::Sense::click()));
    let reset_hovered = reset_resp.as_ref().is_some_and(|r| r.hovered()) || switch_resp.as_ref().is_some_and(|r| r.hovered());
    if let (Some(r), Some(o)) = (&switch_resp, off.as_deref_mut()) {
        if r.clicked() {
            *o = !*o;
        }
    }
    let is_off = off.as_deref().copied().unwrap_or(false);
    // a click on the copy of this header that sticks to the top of the list counts like a click on it
    if (resp.clicked() && !reset_hovered) || crate::sticky::take_click(ui, id) {
        state.toggle(ui);
    }
    // Context menu: reset, copy, paste section (only this section)
    if local.is_none() && crate::paste::for_section(title).is_some() {
        let clip: Option<String> = ui.ctx().data(|d| d.get_temp(egui::Id::unique(SECTION_CLIP)));
        let action = egui::Id::unique(SECTION_ACTION);
        resp.context_menu(|ui| {
            // frequent first, resetting (destructive) last
            if menu_item(ui, crate::tr("Abschnitt kopieren"), "", true) {
                ui.ctx().data_mut(|d| d.insert_temp(action, (title.to_string(), 1u8)));
            }
            let can = clip.as_deref() == Some(title);
            if menu_item(ui, crate::tr("Abschnitt einfügen"), "", can) {
                ui.ctx().data_mut(|d| d.insert_temp(action, (title.to_string(), 2u8)));
            }
            ui.separator();
            if menu_item(ui, crate::tr("Abschnitt zurücksetzen"), "", changed) {
                reset = true;
            }
        });
    }
    let painter = ui.painter();
    if resp.hovered() && !reset_hovered {
        painter.rect_filled(rect, 0.0, Color32::from_gray(0x22));
    }
    // Arrow, rotates when expanding
    let openness = state.openness(ui.ctx());
    let (title_pos, title_w) = section_head_paint(painter, rect, title, openness, resp.hovered());
    {
        // the header sticks to the top of a long list (`sticky`): same look, without reset and switch
        let owned = title.to_string();
        crate::sticky::register(crate::sticky::RANK_SECTION, id, rect, move |p, bar, hovered| {
            if hovered {
                p.rect_filled(bar, 0.0, Color32::from_gray(0x22));
            }
            let (pos, w) = section_head_paint(p, bar, &owned, openness, hovered);
            if changed {
                p.circle_filled(egui::pos2(pos.x + w + 8.0, bar.center().y), 2.5, if is_off { Color32::from_gray(0x70) } else { ACCENT });
            }
        });
    }
    // Help for the section: "?" after the title (and change dot), only while hovering the header
    let edit_section = local.is_none() && crate::paste::for_section(title).is_some();
    if let Some(card) = local.is_none().then(|| crate::help::for_section(title)).flatten().or(edit_section.then_some(&crate::help::EDIT_SECTION)) {
        let q = egui::pos2(title_pos.x + title_w + 22.0, rect.center().y);
        crate::help::question_mark(ui, q, id.with("hilfe"), ui.rect_contains_pointer(rect), title, "", card, if edit_section { SECTION_CONTROLS } else { "" });
    }
    if let Some(r) = &switch_resp {
        // small toggle: light = active, grey = disabled
        let on = !is_off;
        let track = if on { ACCENT.gamma_multiply(0.8) } else { Color32::from_gray(0x62) };
        painter.rect_filled(switch_rect, 6.5, track);
        let knob_x = if on { switch_rect.right() - 6.5 } else { switch_rect.left() + 6.5 };
        painter.circle_filled(egui::pos2(knob_x, switch_rect.center().y), 5.0, Color32::from_gray(if r.hovered() { 0xff } else { 0xe8 }));
    }
    if changed {
        // subtle dot: something in this section is adjusted (grey when disabled)
        painter.circle_filled(egui::pos2(title_pos.x + title_w + 8.0, rect.center().y), 2.5, if is_off { Color32::from_gray(0x70) } else { ACCENT });
        let hov = reset_hovered;
        if hov {
            painter.rect_filled(reset_rect, 0.0, Color32::from_gray(0x44));
        }
        painter.text(reset_rect.center(), egui::Align2::CENTER_CENTER, "↺", egui::FontId::proportional(14.0), if hov { Color32::WHITE } else { TEXT_DIM });
        if let Some(r) = reset_resp {
            hover_tip(ui, &r, crate::tr("Abschnitt zurücksetzen"));
            if r.clicked() {
                reset = true;
            }
        }
    }
    if let Some(r) = &switch_resp {
        hover_tip(ui, r, if is_off { crate::tr("Abschnitt einschalten") } else { crate::tr("Abschnitt vorübergehend ausschalten") });
    }
    state.show_body_unindented(ui, |ui| {
        ui.add_space(2.0);
        crate::sticky::enter();
        ui.indent(id.with("body"), |ui| {
            ui.spacing_mut().indent = 6.0;
            if is_off {
                // disabled: sliders stay usable but look visibly dimmed
                ui.multiply_opacity(0.45);
            }
            bounded(ui, |ui| { add(ui); });
        });
        crate::sticky::leave();
        ui.add_space(6.0);
    });
    state.store(ui.ctx());
    // fine separator line between sections
    let y = ui.cursor().top() + 1.0;
    ui.painter().line_segment([egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)], Stroke::new(1.0, Color32::from_gray(0x27)));
    ui.add_space(3.0);
    reset
}

/// Histogram with clipping triangles on the left (shadows) and right (highlights).
/// Returns: was a triangle clicked (toggle clipping display)?
/// Area of the histogram that can be dragged directly.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum HistRegion {
    Shadows,
    Exposure,
    Highlights,
}

impl HistRegion {
    pub fn label(self) -> &'static str {
        match self {
            HistRegion::Shadows => "Tiefen",
            HistRegion::Exposure => "Belichtung",
            HistRegion::Highlights => "Lichter",
        }
    }
    /// Change of the slider value (shadows, exposure, highlights) for a drag by `dx` (fraction of the width).
    /// All three areas alike: to the right = positive value = brighter.
    pub fn slider_delta(self, dx: f64) -> f64 {
        match self {
            HistRegion::Shadows | HistRegion::Highlights => dx * 4.0,
            HistRegion::Exposure => dx * 3.0,
        }
    }
    fn span(self) -> (f32, f32) {
        match self {
            HistRegion::Shadows => (0.0, 0.3),
            HistRegion::Exposure => (0.3, 0.7),
            HistRegion::Highlights => (0.7, 1.0),
        }
    }
}

#[derive(Default)]
pub struct HistResponse {
    /// Clipping triangle clicked
    pub toggle_clipping: bool,
    /// Dragged: area and shift as a fraction of the width
    pub drag: Option<(HistRegion, f32)>,
    /// Double click outside the clipping triangles (next size of the display)
    pub double_clicked: bool,
}

/// Height of the histogram (and of the other displays at the top of the Edit panel).
pub const HISTOGRAM_H: f32 = 110.0;

pub fn histogram(ui: &mut egui::Ui, h: &[Vec<u32>; 3], show_clipping: bool) -> HistResponse {
    histogram_h(ui, h, show_clipping, HISTOGRAM_H)
}

/// [`histogram`] with its own height (the pinned histogram gets smaller in low windows).
pub fn histogram_h(ui: &mut egui::Ui, h: &[Vec<u32>; 3], show_clipping: bool, height: f32) -> HistResponse {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, PANEL_DARK);
    let n = h[0].len();
    let max = h.iter().flat_map(|v| v[1..n - 1].iter()).copied().max().unwrap_or(1).max(1) as f32;
    let colors = [
        Color32::from_rgba_unmultiplied(220, 70, 70, 120),
        Color32::from_rgba_unmultiplied(70, 200, 90, 120),
        Color32::from_rgba_unmultiplied(80, 120, 235, 120),
    ];
    for (c, bins) in h.iter().enumerate() {
        let mut pts = vec![rect.left_bottom()];
        for (i, &v) in bins.iter().enumerate() {
            let x = rect.left() + rect.width() * i as f32 / (n - 1) as f32;
            pts.push(egui::pos2(x, rect.bottom() - rect.height() * (v as f32 / max).min(1.0)));
        }
        pts.push(rect.right_bottom());
        painter.add(egui::Shape::line(pts, Stroke::new(1.2, colors[c].to_opaque().gamma_multiply(0.9))));
    }
    // Clipping triangles: bright when clipped; outlined when the display is active
    let total = h[1].iter().sum::<u32>().max(1) as f32;
    let lo = h.iter().map(|v| v[0]).max().unwrap_or(0) as f32 / total;
    let hi = h.iter().map(|v| v[n - 1]).max().unwrap_or(0) as f32 / total;
    let tri = |at: egui::Pos2, right: bool, lit: bool| {
        let s = 7.0;
        let pts = if right {
            vec![at, at + egui::vec2(-s, 0.0), at + egui::vec2(0.0, s)]
        } else {
            vec![at, at + egui::vec2(s, 0.0), at + egui::vec2(0.0, s)]
        };
        let fill = if lit { Color32::from_gray(0xee) } else { Color32::from_gray(0x55) };
        let stroke = if show_clipping { Stroke::new(1.0, ACCENT) } else { Stroke::NONE };
        painter.add(egui::Shape::convex_polygon(pts, fill, stroke));
    };
    tri(rect.left_top() + egui::vec2(3.0, 3.0), false, lo > 0.001);
    tri(rect.right_top() + egui::vec2(-3.0, 3.0), true, hi > 0.001);

    // Highlight the area under the mouse pointer; while dragging, the start area stays active
    let mut out = HistResponse::default();
    let region_id = resp.id.with("region");
    let at = |p: egui::Pos2| {
        let u = (p.x - rect.left()) / rect.width();
        [HistRegion::Shadows, HistRegion::Exposure, HistRegion::Highlights].into_iter().find(|r| u < r.span().1).unwrap_or(HistRegion::Highlights)
    };
    if resp.drag_started() {
        if let Some(p) = resp.interact_pointer_pos() {
            ui.data_mut(|d| d.insert_temp(region_id, at(p) as u8));
        }
    }
    let region = if resp.dragged() {
        ui.data(|d| d.get_temp::<u8>(region_id)).map(|r| [HistRegion::Shadows, HistRegion::Exposure, HistRegion::Highlights][r as usize])
    } else {
        resp.hover_pos().filter(|p| p.y > rect.top() + 14.0).map(at)
    };
    let corner = |p: egui::Pos2| p.y < rect.top() + 14.0 && (p.x < rect.left() + 14.0 || p.x > rect.right() - 14.0);
    if let Some(r) = region {
        let (a, b) = r.span();
        let band = egui::Rect::from_x_y_ranges(rect.left() + rect.width() * a..=rect.left() + rect.width() * b, rect.y_range());
        painter.rect_filled(band, 0.0, Color32::from_white_alpha(10));
        painter.text(rect.left_bottom() + egui::vec2(6.0, -4.0), egui::Align2::LEFT_BOTTOM, r.label(), egui::TextStyle::Small.resolve(ui.style()), TEXT);
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        if resp.dragged() {
            out.drag = Some((r, resp.drag_delta().x / rect.width()));
        }
    }
    let clicked = resp.clicked() && resp.interact_pointer_pos().is_some_and(corner);
    let hovered = resp.hovered();
    crate::help::question_mark(ui, rect.right_bottom() + egui::vec2(-10.0, -10.0), resp.id.with("hilfe"), hovered, "Histogramm", "", &crate::help::HISTOGRAM, "");
    out.toggle_clipping = clicked;
    out.double_clicked = resp.double_clicked() && !resp.interact_pointer_pos().is_some_and(corner);
    out
}

/// RGB parade or waveform in the sensor module (choice is remembered).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Scope {
    #[default]
    Parade,
    Waveform,
    /// Waveform with red, green and blue overlaid (colored traces)
    RgbWaveform,
}

impl Scope {
    pub const ALL: [Scope; 3] = [Scope::Parade, Scope::Waveform, Scope::RgbWaveform];
    pub fn label(self) -> &'static str {
        match self {
            Scope::Parade => "RGB-Parade",
            Scope::Waveform => "Waveform",
            Scope::RgbWaveform => "RGB-Waveform",
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Scope::Parade => "parade",
            Scope::Waveform => "waveform",
            Scope::RgbWaveform => "rgb-waveform",
        }
    }
    pub fn from_key(s: &str) -> Self {
        Self::ALL.into_iter().find(|m| m.key() == s.trim()).unwrap_or_default()
    }
}

const SCOPE_COLS: usize = 384;
const SCOPE_ROWS: usize = 128;

/// Waveform or RGB parade of the 8-bit preview image as an image (column = image column,
/// height = tone value, brightness = frequency). One pass over the pixels, a few ms.
pub fn scope_image(rgb: &[u8], width: usize, height: usize, scope: Scope) -> egui::ColorImage {
    let (cols, rows) = (SCOPE_COLS, SCOPE_ROWS);
    let parade = scope == Scope::Parade;
    let rgb_wave = scope == Scope::RgbWaveform;
    // Count grid: waveform has one (luminance), parade three side by side (cols/3 columns each)
    let lane = if parade { cols / 3 } else { cols };
    let mut counts = vec![[0u32; 3]; cols * rows];
    if width > 0 && height > 0 {
        let col_of: Vec<usize> = (0..width).map(|x| x * lane / width).collect();
        let row = |v: u8| (rows - 1) - (v as usize * rows / 256);
        for y in 0..height {
            let line = &rgb[y * width * 3..(y + 1) * width * 3];
            for (x, p) in line.chunks_exact(3).enumerate() {
                let c = col_of[x];
                if parade || rgb_wave {
                    let off = |k: usize| if parade { k * lane } else { 0 };
                    for k in 0..3 {
                        counts[row(p[k]) * cols + off(k) + c][k] += 1;
                    }
                } else {
                    // Luma Rec. 709 on the display values, as in video editors
                    let l = (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32 + 0.5) as u8;
                    let cell = &mut counts[row(l) * cols + c];
                    cell[0] += 1;
                }
            }
        }
    }
    // mean occupancy per cell as the scale → bright traces even for large images
    let mean = (width as f32 * height as f32 / (lane as f32 * rows as f32)).max(1.0);
    let lum = |n: u32| ((n as f32 / mean).sqrt() * 0.32).min(1.0);
    let tint: [[f32; 3]; 3] = [[1.0, 0.32, 0.30], [0.35, 1.0, 0.40], [0.38, 0.55, 1.0]];
    let pixels = counts
        .iter()
        .map(|c| {
            if parade || rgb_wave {
                let mut o = [0.0f32; 3];
                for k in 0..3 {
                    let v = lum(c[k]);
                    for j in 0..3 {
                        o[j] += v * tint[k][j];
                    }
                }
                Color32::from_rgb((o[0].min(1.0) * 255.0) as u8, (o[1].min(1.0) * 255.0) as u8, (o[2].min(1.0) * 255.0) as u8)
            } else {
                let v = lum(c[0]);
                Color32::from_rgb((v * 225.0) as u8, (v * 240.0) as u8, (v * 225.0) as u8)
            }
        })
        .collect();
    egui::ColorImage { size: [cols, rows], source_size: egui::vec2(cols as f32, rows as f32), pixels }
}

/// Waveform/parade, same height as the histograms (no jumping when switching).
pub fn scope_view(ui: &mut egui::Ui, tex: Option<&egui::TextureHandle>, scope: Scope) {
    let _ = scope_view_h(ui, tex, scope, 96.0, false);
}

/// Like [`scope_view`], with selectable height and optionally a percent scale on the left (0 … 100 %).
/// Returns: double clicked (next size of the display).
pub fn scope_view_h(ui: &mut egui::Ui, tex: Option<&egui::TextureHandle>, scope: Scope, height: f32, scale: bool) -> bool {
    let (outer, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::click());
    let rect = if scale { egui::Rect::from_min_max(outer.min + egui::vec2(34.0, 6.0), outer.max - egui::vec2(0.0, 6.0)) } else { outer };
    if scale {
        let p = ui.painter();
        p.rect_filled(outer, 0.0, PANEL_DARK);
        for q in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
            let y = rect.bottom() - rect.height() * q;
            p.text(egui::pos2(outer.left() + 30.0, y), egui::Align2::RIGHT_CENTER, format!("{:.0}%", q * 100.0), egui::FontId::proportional(10.0), Color32::from_gray(0x70));
        }
    }
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, PANEL_DARK);
    if let Some(t) = tex {
        painter.image(t.id(), rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
    }
    // Guide lines at 25/50/75 %, for the parade also separators between the channels
    let grid = Stroke::new(1.0, Color32::from_white_alpha(14));
    let qs: &[f32] = if scale { &[0.0, 0.25, 0.5, 0.75, 1.0] } else { &[0.25, 0.5, 0.75] };
    for &q in qs {
        let y = rect.bottom() - rect.height() * q;
        painter.hline(rect.x_range(), y, grid);
    }
    if scope == Scope::Parade {
        for q in [1.0 / 3.0, 2.0 / 3.0] {
            painter.vline(rect.left() + rect.width() * q, rect.y_range(), Stroke::new(1.0, Color32::from_white_alpha(28)));
        }
    }
    let (title, card) = match scope {
        Scope::Parade => ("RGB-Parade", &crate::help::SCOPE_PARADE),
        Scope::RgbWaveform => ("RGB-Waveform", &crate::help::SCOPE_RGB_WAVE),
        _ => ("Waveform", &crate::help::SCOPE_WAVE),
    };
    crate::help::question_mark(ui, outer.right_top() + egui::vec2(-10.0, 10.0), resp.id.with("hilfe"), resp.hovered(), title, "", card, "");
    resp.double_clicked()
}

/// Color wheel for color grading: angle = hue, distance from the centre = saturation.
/// Dragging sets both, ⇧ while dragging locks the hue, double-click = neutral.
pub fn color_wheel(ui: &mut egui::Ui, label: &str, value: &mut [f32; 2], size: f32) -> bool {
    let before = *value;
    ui.vertical(|ui| {
        ui.set_width(size);
        // fixed ID per wheel (see tone_curve)
        let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
        let resp = ui.interact(rect, egui::Id::unique(("lr-color-wheel", label)), egui::Sense::click_and_drag());
        let c = rect.center();
        let r = size * 0.5 - 3.0;
        let hsv = |h: f32, s: f32| {
            let col = egui::ecolor::Hsva::new(h / 360.0, s * 0.85, 0.62, 1.0);
            Color32::from(col)
        };
        let mut mesh = egui::Mesh::default();
        mesh.colored_vertex(c, Color32::from_gray(0x9e));
        let n = 72;
        for i in 0..=n {
            let a = i as f32 / n as f32 * std::f32::consts::TAU;
            // 0° = red on the right, counterclockwise
            mesh.colored_vertex(c + egui::vec2(a.cos(), -a.sin()) * r, hsv(a.to_degrees(), 1.0));
        }
        for i in 1..=n as u32 {
            mesh.add_triangle(0, i, i + 1);
        }
        ui.painter().add(egui::Shape::mesh(mesh));
        ui.painter().circle_stroke(c, r, Stroke::new(1.0, Color32::from_gray(0x50)));
        let shift = ui.input(|i| i.modifiers.shift);
        if resp.double_clicked() {
            crate::history::reset_gesture();
            value[1] = 0.0;
        } else if let Some(p) = resp.interact_pointer_pos().filter(|_| resp.dragged() || resp.clicked()) {
            let d = p - c;
            let sat = (d.length() / r).clamp(0.0, 1.0);
            if !shift {
                value[0] = (-d.y).atan2(d.x).to_degrees().rem_euclid(360.0);
            }
            value[1] = sat;
        }
        let a = value[0].to_radians();
        let dot = c + egui::vec2(a.cos(), -a.sin()) * r * value[1];
        ui.painter().circle(dot, 4.0, hsv(value[0], value[1].max(0.3)), Stroke::new(1.5, Color32::WHITE));
        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        let txt = if value[1] > 0.0 { format!("{label}  {:.0}° · {:.0} %", value[0], value[1] * 100.0) } else { label.to_string() };
        ui.vertical_centered(|ui| ui.label(egui::RichText::new(txt).small().color(if value[1] > 0.0 { TEXT } else { TEXT_DIM })));
    });
    *value != before
}

/// Light subsection (e.g. preset folder): small arrow, semibold title, count.
pub fn subsection<R>(ui: &mut egui::Ui, title: &str, count: usize, open: bool, add: impl FnOnce(&mut egui::Ui) -> R) {
    let id = ui.make_persistent_id(("lr-subsection", title));
    let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, open);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 24.0), egui::Sense::click());
    if resp.clicked() {
        state.toggle(ui);
    }
    let painter = ui.painter();
    if resp.hovered() {
        painter.rect_filled(rect, 0.0, Color32::from_gray(0x22));
    }
    let openness = state.openness(ui.ctx());
    let c = egui::pos2(rect.left() + 9.0, rect.center().y);
    let a = openness * std::f32::consts::FRAC_PI_2;
    let rot = |p: egui::Vec2| c + egui::vec2(p.x * a.cos() - p.y * a.sin(), p.x * a.sin() + p.y * a.cos());
    let arrow = vec![rot(egui::vec2(-2.0, -3.2)), rot(egui::vec2(2.8, 0.0)), rot(egui::vec2(-2.0, 3.2))];
    painter.add(egui::Shape::convex_polygon(arrow, if resp.hovered() { Color32::from_gray(0xe0) } else { Color32::from_gray(0x8a) }, Stroke::NONE));
    let color = if resp.hovered() { Color32::WHITE } else { Color32::from_gray(0xd0) };
    painter.text(egui::pos2(rect.left() + 20.0, rect.center().y), egui::Align2::LEFT_CENTER, crate::tr(title), semibold(12.5), color);
    painter.text(egui::pos2(rect.right() - 6.0, rect.center().y), egui::Align2::RIGHT_CENTER, count.to_string(), egui::TextStyle::Small.resolve(ui.style()), TEXT_DIM);
    state.show_body_unindented(ui, |ui| {
        ui.indent(id.with("body"), |ui| {
            ui.spacing_mut().indent = 10.0;
            bounded(ui, add)
        });
    });
    state.store(ui.ctx());
}

/// Curve color per channel (RGB, red, green, blue).
const CURVE_COLORS: [Color32; 4] = [
    Color32::from_gray(0xdc),
    Color32::from_rgb(0xe0, 0x62, 0x58),
    Color32::from_rgb(0x6c, 0xc4, 0x64),
    Color32::from_rgb(0x6c, 0x9a, 0xea),
];

/// Tone curve (point curve) for channel `channel` (0 = RGB, 1 … 3 =
/// red, green, blue): square field with grid and a subtle histogram behind it.
/// Clicking sets a point on the curve, dragging moves it, double-click or dragging out of
/// the field removes it (end points: double-click resets them). Below is a fixed row
/// with input/output (0 … 255). Returns: changed?
pub fn tone_curve(ui: &mut egui::Ui, curve: &mut develop::ToneCurve, channel: usize, hist: Option<&[Vec<u32>; 3]>) -> bool {
    use develop::curve::{is_identity, Spline};
    let before = curve.clone();
    let size = ui.available_width().min(280.0).max(120.0);
    // fixed ID: the section header above gets additional controls on a change –
    // an automatic ID would then change and abort the drag
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let resp = ui.interact(rect, egui::Id::unique("lr-tone-curve"), egui::Sense::click_and_drag());
    let painter = ui.painter_at(rect.expand(6.0));
    let to_screen = |p: [f32; 2]| egui::pos2(rect.left() + p[0] * rect.width(), rect.bottom() - p[1] * rect.height());
    let from_screen = |q: egui::Pos2| [((q.x - rect.left()) / rect.width()).clamp(0.0, 1.0), ((rect.bottom() - q.y) / rect.height()).clamp(0.0, 1.0)];
    let color = CURVE_COLORS[channel.min(3)];

    // Background, histogram, grid
    painter.rect_filled(rect, 0.0, PANEL_DARK);
    if let Some(h) = hist {
        let n = h[0].len();
        let bins: Vec<f32> = (0..n)
            .map(|i| if channel == 0 { (h[0][i] + h[1][i] + h[2][i]) as f32 / 3.0 } else { h[channel - 1][i] as f32 })
            .collect();
        let max = bins[1..n - 1].iter().copied().fold(1.0f32, f32::max);
        let fill = if channel == 0 { Color32::from_white_alpha(18) } else { color.gamma_multiply(0.16) };
        for (i, &v) in bins.iter().enumerate() {
            let hgt = (v / max).min(1.0).sqrt() * rect.height() * 0.85;
            if hgt < 0.5 {
                continue;
            }
            let x0 = rect.left() + rect.width() * i as f32 / n as f32;
            let x1 = rect.left() + rect.width() * (i + 1) as f32 / n as f32;
            painter.rect_filled(egui::Rect::from_min_max(egui::pos2(x0, rect.bottom() - hgt), egui::pos2(x1 + 0.3, rect.bottom())), 0.0, fill);
        }
    }
    let grid = Stroke::new(1.0, Color32::from_gray(0x38));
    for k in 1..4 {
        let t = k as f32 / 4.0;
        painter.vline(rect.left() + t * rect.width(), rect.y_range(), grid);
        painter.hline(rect.x_range(), rect.top() + t * rect.height(), grid);
    }
    painter.line_segment([rect.left_bottom(), rect.right_top()], Stroke::new(1.0, Color32::from_gray(0x44)));
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0, Color32::from_gray(0x4a)), egui::StrokeKind::Inside);

    // Interaction: pressing selects the nearest point or sets a new one on the curve
    let mut work: Vec<[f32; 2]> = {
        let p = curve.channel(channel);
        if p.is_empty() { vec![[0.0, 0.0], [1.0, 1.0]] } else { p.clone() }
    };
    let drag_id = resp.id.with("punkt");
    let mut drag: Option<usize> = ui.data(|d| d.get_temp(drag_id));
    let pointer = ui.input(|i| i.pointer.interact_pos().or(i.pointer.hover_pos()));
    let nearest = |work: &[[f32; 2]], q: egui::Pos2| -> Option<usize> {
        work.iter()
            .enumerate()
            .map(|(i, p)| (i, to_screen(*p).distance(q)))
            .filter(|(_, d)| *d <= 9.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    };
    let mut changed_pts = false;
    if resp.double_clicked() {
        if let Some(i) = pointer.and_then(|q| nearest(&work, q)) {
            let n = work.len();
            if i == 0 {
                work[0] = [0.0, 0.0];
            } else if i == n - 1 {
                work[n - 1] = [1.0, 1.0];
            } else {
                work.remove(i);
            }
            changed_pts = true;
        }
        drag = None;
    } else if resp.hovered() && ui.input(|i| i.pointer.primary_pressed()) {
        if let Some(q) = pointer {
            drag = nearest(&work, q).or_else(|| {
                let x = from_screen(q)[0];
                let at = work.partition_point(|p| p[0] < x);
                let gap_ok = (at == 0 || x - work[at - 1][0] >= 0.01) && (at == work.len() || work[at][0] - x >= 0.01);
                (gap_ok && at > 0 && at < work.len()).then(|| {
                    let y = Spline::new(&work).map_or(x, |s| s.eval(x));
                    work.insert(at, [x, y]);
                    changed_pts = true;
                    at
                })
            });
        }
    }
    if let Some(i) = drag.filter(|&i| i < work.len()) {
        if ui.input(|inp| inp.pointer.primary_down()) {
            if let Some(q) = ui.input(|inp| inp.pointer.interact_pos()) {
                let n = work.len();
                let outside = !rect.expand(24.0).contains(q);
                if outside && i > 0 && i < n - 1 {
                    // dragged out of the field: remove the point
                    work.remove(i);
                    drag = None;
                } else {
                    let p = from_screen(q);
                    let lo = if i == 0 { 0.0 } else { work[i - 1][0] + 0.01 };
                    let hi = if i == n - 1 { 1.0 } else { work[i + 1][0] - 0.01 };
                    work[i] = [p[0].clamp(lo, hi.max(lo)), p[1]];
                }
                changed_pts = true;
            }
        } else {
            drag = None;
        }
    } else {
        drag = None;
    }
    // store as `usize` (it is read as `usize` above as well) – an `Option<usize>` here
    // would never be found again, the drag would end after the first frame
    ui.data_mut(|d| match drag {
        Some(i) => {
            d.insert_temp(drag_id, i);
        }
        None => {
            d.remove::<usize>(drag_id);
        }
    });
    if changed_pts {
        // Only the two end points on the diagonal = no curve
        if work.len() == 2 && is_identity(&work) {
            work.clear();
        }
        *curve.channel_mut(channel) = work.clone();
        if work.is_empty() {
            work = vec![[0.0, 0.0], [1.0, 1.0]];
        }
    }

    // Other channels with an effect faintly behind, the active channel strong
    let samples = |p: &[[f32; 2]]| -> Vec<egui::Pos2> {
        let sp = Spline::new(p);
        (0..=128).map(|k| {
            let x = k as f32 / 128.0;
            to_screen([x, sp.as_ref().map_or(x, |s| s.eval(x))])
        }).collect()
    };
    for k in 0..4 {
        if k != channel && !is_identity(curve.channel(k)) {
            painter.add(egui::Shape::line(samples(curve.channel(k)), Stroke::new(1.0, CURVE_COLORS[k].gamma_multiply(0.35))));
        }
    }
    painter.add(egui::Shape::line(samples(&work), Stroke::new(1.6, color)));
    let hover_pt = if drag.is_none() { pointer.filter(|_| resp.hovered()).and_then(|q| nearest(&work, q)) } else { None };
    for (i, p) in work.iter().enumerate() {
        let active = drag == Some(i) || hover_pt == Some(i);
        let fill = if active { color } else { PANEL_DARK };
        painter.circle(to_screen(*p), if active { 4.5 } else { 3.5 }, fill, Stroke::new(1.4, color));
    }
    if drag.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    } else if hover_pt.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    } else if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }

    // Fixed value row – nothing below it shifts
    let (text, bright) = if let Some(i) = drag.filter(|&i| i < work.len()) {
        (crate::trf!("Eingabe {a}  ·  Ausgabe {b}", a = format!("{:.0}", work[i][0] * 255.0), b = format!("{:.0}", work[i][1] * 255.0)), true)
    } else if let Some(q) = pointer.filter(|_| resp.hovered()) {
        let x = from_screen(q)[0];
        (crate::trf!("Eingabe {a}  ·  Ausgabe {b}", a = format!("{:.0}", x * 255.0), b = format!("{:.0}", develop::curve::eval(&work, x) * 255.0)), false)
    } else {
        ("Klicken: Punkt · Ziehen · Doppelklick entfernt".to_string(), false)
    };
    ui.allocate_ui_with_layout(egui::vec2(size, 16.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.add(egui::Label::new(egui::RichText::new(crate::tr(&text)).small().color(if bright { TEXT } else { TEXT_DIM })).truncate());
    });
    *curve != before
}

/// Icons from the free Phosphor Icons set (weight "Light", MIT license, embedded).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Icon {
    Camera,
    Auto,
    Sun,
    Cloud,
    Shade,
    Bulb,
    Tube,
    Flash,
    Custom,
    Picker,
    Pencil,
    Chevron,
    Film,
    /// Crop: level horizontally/vertically, straighten (ruler)
    ArrowsH,
    ArrowsV,
    Ruler,
    Lock,
    LockOpen,
    Perspective,
    /// Tool rail on the right
    Mask,
    Sparkle,
    /// Darkroom toggle
    CornersOut,
    CopySimple,
    SquaresFour,
    ImageSquare,
    /// Workspace and sensor view
    Info,
    Sliders,
    Grid,
    Image,
    Export,
    Crosshair,
    Aperture,
    Waveform,
    Target,
    Warning,
    Checkerboard,
    Drop,
    CircleHalf,
    ChartLine,
    GridNine,
    Stack,
    XSquare,
    Crop,
    SplitH,
    RotCw,
    RotCcw,
    Star,
    Flag,
    X,
    Before,
    ZoomIn,
    ZoomOut,
    FolderOpen,
    Smiley,
    /// Filter bar of the library
    Funnel,
    Video,
    Images,
    SortDown,
    SortUp,
    Minus,
    /// Special rows of the library's left panel
    PlusCircle,
    TrayArrowDown,
    ImageBroken,
    LinkBreak,
    Copy,
    User,
    FolderDashed,
    /// Flag states: pick, none, reject
    Check,
    CheckCircle,
    CircleDashed,
    XCircle,
    /// Zoom: fit the photo
    FrameCorners,
    /// Pin of the histogram at the top of the Edit trays
    PushPin,
    /// Retouch tool of the rail
    Bandaids,
    /// Masks tray: on/off, menu, combination of parts
    Eye,
    EyeSlash,
    DotsThree,
    Plus,
    Intersect,
    /// Masks tray: kinds of new masks
    UserFocus,
    Users,
    CursorClick,
    PaintBrush,
    Gradient,
    Circle,
    SunDim,
    Palette,
}

impl Icon {
    /// Glyph in the Phosphor font
    pub fn glyph(self) -> char {
        match self {
            Icon::Camera => '\u{e10e}',
            Icon::Auto => '\u{e6b6}',
            Icon::Sun => '\u{e472}',
            Icon::Cloud => '\u{e1aa}',
            Icon::Shade => '\u{e2c2}',
            Icon::Bulb => '\u{e63c}',
            Icon::Tube => '\u{e638}',
            Icon::Flash => '\u{e2de}',
            Icon::Custom => '\u{e4cc}',
            Icon::Picker => '\u{e568}',
            Icon::Pencil => '\u{e3b4}',
            Icon::Chevron => '\u{e13a}',
            Icon::Film => '\u{e792}',
            Icon::ArrowsH => '\u{eb06}',
            Icon::ArrowsV => '\u{eb04}',
            Icon::Ruler => '\u{e6b8}',
            Icon::Lock => '\u{e308}',
            Icon::LockOpen => '\u{e30a}',
            Icon::Perspective => '\u{ebe6}',
            Icon::Mask => '\u{e602}',
            Icon::Sparkle => '\u{e6a2}',
            Icon::CornersOut => '\u{e1d0}',
            Icon::CopySimple => '\u{e1cc}',
            Icon::SquaresFour => '\u{e464}',
            Icon::ImageSquare => '\u{e2cc}',
            Icon::Info => '\u{e2ce}',
            Icon::Sliders => '\u{e434}',
            Icon::Grid => '\u{e296}',
            Icon::Image => '\u{e2ca}',
            Icon::Export => '\u{eaf0}',
            Icon::Crosshair => '\u{e1d6}',
            Icon::Aperture => '\u{e00a}',
            Icon::Waveform => '\u{e802}',
            Icon::Target => '\u{e47c}',
            Icon::Warning => '\u{e4e0}',
            Icon::Smiley => '\u{e436}',
            Icon::Checkerboard => '\u{e8c4}',
            Icon::Drop => '\u{e210}',
            Icon::CircleHalf => '\u{e18c}',
            Icon::ChartLine => '\u{e154}',
            Icon::GridNine => '\u{ec8c}',
            Icon::Stack => '\u{e466}',
            Icon::XSquare => '\u{e4fa}',
            Icon::Crop => '\u{e1d4}',
            Icon::SplitH => '\u{e870}',
            Icon::RotCw => '\u{e036}',
            Icon::RotCcw => '\u{e038}',
            Icon::Star => '\u{e46a}',
            Icon::Flag => '\u{e244}',
            Icon::X => '\u{e4f6}',
            Icon::Before => '\u{e1a0}',
            Icon::ZoomIn => '\u{e310}',
            Icon::ZoomOut => '\u{e30e}',
            Icon::FolderOpen => '\u{e256}',
            Icon::Funnel => '\u{e266}',
            Icon::Video => '\u{e4da}',
            Icon::Images => '\u{e836}',
            Icon::SortDown => '\u{e444}',
            Icon::SortUp => '\u{e446}',
            Icon::Minus => '\u{e32a}',
            Icon::PlusCircle => '\u{e3d6}',
            Icon::TrayArrowDown => '\u{e010}',
            Icon::ImageBroken => '\u{e7a8}',
            Icon::LinkBreak => '\u{e2e4}',
            Icon::Copy => '\u{e1ca}',
            Icon::User => '\u{e4c2}',
            Icon::FolderDashed => '\u{e8f8}',
            Icon::Check => '\u{e182}',
            Icon::CheckCircle => '\u{e184}',
            Icon::CircleDashed => '\u{e602}',
            Icon::XCircle => '\u{e4f8}',
            Icon::FrameCorners => '\u{e626}',
            Icon::PushPin => '\u{e3e2}',
            Icon::Bandaids => '\u{e0b2}',
            Icon::Eye => '\u{e220}',
            Icon::EyeSlash => '\u{e224}',
            Icon::DotsThree => '\u{e1fe}',
            Icon::Plus => '\u{e3d4}',
            Icon::Intersect => '\u{e2d2}',
            Icon::UserFocus => '\u{e6fc}',
            Icon::Users => '\u{e4d6}',
            Icon::CursorClick => '\u{e7c8}',
            Icon::PaintBrush => '\u{e6f0}',
            Icon::Gradient => '\u{eb42}',
            Icon::Circle => '\u{e18a}',
            Icon::SunDim => '\u{e474}',
            Icon::Palette => '\u{e6c8}',
        }
    }
}

/// Symbol of a flag state (tick / circle / cross): `flag` 1 pick (tick in a
/// circle), −1 rejected (cross in a circle), 0 none (dashed circle). `filled`: the active state – a full disc in
/// `color` with the tick or cross cut out dark. `size`: edge of the square the symbol fills (like the other icons).
pub fn paint_flag(painter: &egui::Painter, center: egui::Pos2, size: f32, flag: i8, filled: bool, color: Color32) {
    let r = egui::Rect::from_center_size(center, egui::vec2(size, size));
    match (flag, filled) {
        (1 | -1, true) => {
            painter.circle_filled(center, size * 0.42, color);
            let mark = if flag == 1 { Icon::Check } else { Icon::X };
            paint_icon(painter, egui::Rect::from_center_size(center, egui::vec2(size * 0.62, size * 0.62)), mark, Color32::from_gray(0x16));
        }
        (1, false) => paint_icon(painter, r, Icon::CheckCircle, color),
        (-1, false) => paint_icon(painter, r, Icon::XCircle, color),
        _ => paint_icon(painter, r, Icon::CircleDashed, color),
    }
}

/// Round dark plaque behind a symbol on the image: a disc concentric with the symbol, `radius` of
/// the symbol plus a rim of 3.5 pt – the same for every badge on photos (quick collection, flag).
pub fn image_plaque(painter: &egui::Painter, center: egui::Pos2, radius: f32) {
    painter.circle_filled(center, radius + 3.5, Color32::from_black_alpha(170));
}

/// Space kept free beside a shown scroll bar (bar and gap), see the scroll style in [`apply_theme`].
pub const SCROLL_BAND: f32 = 9.0;

/// Font of the icons (Phosphor Light).
pub fn icon_font(size: f32) -> egui::FontId {
    egui::FontId::new(size, egui::FontFamily::Name("icons".into()))
}

/// Paints `icon` centred in `rect`.
pub fn paint_icon(painter: &egui::Painter, rect: egui::Rect, icon: Icon, color: Color32) {
    painter.text(rect.center(), egui::Align2::CENTER_CENTER, icon.glyph(), icon_font(rect.height().max(10.0)), color);
}

/// Grid of icon buttons with capital labels below (e.g. white balance presets).
/// Fixed column count so nothing jumps. Returns: clicked entry.
pub fn icon_grid(ui: &mut egui::Ui, id: &str, items: &[(Icon, &str, &str)], selected: Option<usize>, cols: usize) -> Option<usize> {
    let w = ui.available_width();
    let gap = 4.0;
    let cell_w = (w - gap * (cols as f32 - 1.0)) / cols as f32;
    let cell_h = 38.0;
    let rows = items.len().div_ceil(cols);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, rows as f32 * cell_h + (rows as f32 - 1.0) * gap), egui::Sense::hover());
    let mut clicked = None;
    for (i, (icon, label, tip)) in items.iter().enumerate() {
        let (r, c) = (i / cols, i % cols);
        let cell = egui::Rect::from_min_size(rect.min + egui::vec2(c as f32 * (cell_w + gap), r as f32 * (cell_h + gap)), egui::vec2(cell_w, cell_h));
        let resp = ui.interact(cell, ui.scope_id().with((id, i)), egui::Sense::click());
        let on = selected == Some(i);
        let painter = ui.painter();
        let (fill, stroke) = if on {
            (Color32::from_gray(0x2c), Color32::from_gray(0x8a))
        } else if resp.hovered() {
            (Color32::from_gray(0x22), Color32::from_gray(0x44))
        } else {
            (Color32::TRANSPARENT, Color32::from_gray(0x2c))
        };
        painter.rect_filled(cell, 0.0, fill);
        painter.rect_stroke(cell, 0.0, Stroke::new(1.0, stroke), egui::StrokeKind::Inside);
        let color = if on { Color32::WHITE } else if resp.hovered() { Color32::from_gray(0xe0) } else { Color32::from_gray(0x9a) };
        paint_icon(painter, egui::Rect::from_center_size(cell.center_top() + egui::vec2(0.0, 14.0), egui::vec2(17.0, 17.0)), *icon, color);
        let g = caps_spaced(painter, label, egui::FontId::proportional(10.0), color, 0.6);
        painter.galley(egui::pos2(cell.center().x - g.size().x / 2.0, cell.bottom() - 4.0 - g.size().y), g, color);
        if resp.clicked() {
            clicked = Some(i);
        }
        if !tip.is_empty() {
            hover_tip(ui, &resp, tip);
        }
    }
    clicked
}

/// Row of round color dots (e.g. black & white filters); the selected one gets a ring.
/// Returns: clicked entry.
pub fn color_chips(ui: &mut egui::Ui, id: &str, items: &[(Color32, &str)], selected: Option<usize>) -> Option<usize> {
    let d = 16.0;
    let gap = 10.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), d + 8.0), egui::Sense::hover());
    let mut clicked = None;
    for (i, (color, tip)) in items.iter().enumerate() {
        let c = egui::pos2(rect.left() + 4.0 + d / 2.0 + i as f32 * (d + gap), rect.center().y);
        let resp = ui.interact(egui::Rect::from_center_size(c, egui::vec2(d + 6.0, d + 6.0)), ui.scope_id().with((id, i)), egui::Sense::click());
        let painter = ui.painter();
        if selected == Some(i) {
            painter.circle_stroke(c, d / 2.0 + 3.0, Stroke::new(1.5, Color32::from_gray(0xe8)));
        } else if resp.hovered() {
            painter.circle_stroke(c, d / 2.0 + 3.0, Stroke::new(1.0, Color32::from_gray(0x60)));
        }
        painter.circle_filled(c, d / 2.0, *color);
        painter.circle_stroke(c, d / 2.0, Stroke::new(1.0, Color32::from_black_alpha(90)));
        if resp.clicked() {
            clicked = Some(i);
        }
        hover_tip(ui, &resp, tip);
    }
    clicked
}

/// "Edited" badge (pencil on a dark field) at the lower right corner `corner`; with `film` also a filmstrip icon to its left.
pub fn edit_badge_film(painter: &egui::Painter, corner: egui::Pos2, film: bool) {
    let r = egui::Rect::from_min_max(corner - egui::vec2(17.0, 17.0), corner - egui::vec2(3.0, 3.0));
    painter.rect_filled(r, 0.0, Color32::from_black_alpha(170));
    paint_icon(painter, r.shrink(1.5), Icon::Pencil, Color32::from_gray(0xe8));
    if film {
        let f = r.translate(egui::vec2(-16.0, 0.0));
        painter.rect_filled(f, 0.0, Color32::from_black_alpha(170));
        paint_icon(painter, f.shrink(1.5), Icon::Film, Color32::from_gray(0xe8));
    }
}

/// Area of the quick collection badge at the top right of a photo (`image`): a dark translucent plaque like
/// the edit badges at the bottom right, so the dot stays readable on a bright sky as on a dark ground.
pub fn quick_badge_rect(image: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_max(egui::pos2(image.right() - 17.0, image.top() + 3.0), egui::pos2(image.right() - 3.0, image.top() + 17.0))
}

/// Quick collection badge: in the collection a white filled dot; `offer` (hover, not in it) an empty ring for adding.
pub fn quick_badge(painter: &egui::Painter, image: egui::Rect, inside: bool, hovered: bool) {
    let r = quick_badge_rect(image);
    let c = r.center();
    image_plaque(painter, c, 4.5);
    if inside {
        painter.circle_filled(c, 4.0, if hovered { Color32::WHITE } else { Color32::from_gray(0xe8) });
    } else {
        painter.circle_stroke(c, 4.0, egui::Stroke::new(1.2, if hovered { Color32::WHITE } else { Color32::from_gray(0xb0) }));
    }
}

/// Grid of aspect ratios (crop): per cell a rectangle in that ratio and the
/// label below. `ratios[i]` is landscape (≥ 1) or `None` for "Free"; `landscape` determines
/// the orientation of the rectangles. Returns: clicked entry.
pub fn aspect_grid(ui: &mut egui::Ui, items: &[(Option<f32>, &str)], selected: Option<usize>, landscape: bool) -> Option<usize> {
    let cols = 5;
    let w = ui.available_width();
    let gap = 4.0;
    let cell_w = (w - gap * (cols as f32 - 1.0)) / cols as f32;
    let cell_h = 40.0;
    let rows = items.len().div_ceil(cols);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, rows as f32 * cell_h + (rows as f32 - 1.0) * gap), egui::Sense::hover());
    let mut clicked = None;
    for (i, (ratio, label)) in items.iter().enumerate() {
        let (r, c) = (i / cols, i % cols);
        let cell = egui::Rect::from_min_size(rect.min + egui::vec2(c as f32 * (cell_w + gap), r as f32 * (cell_h + gap)), egui::vec2(cell_w, cell_h));
        let resp = ui.interact(cell, ui.scope_id().with(("aspect", i)), egui::Sense::click());
        let on = selected == Some(i);
        let painter = ui.painter();
        if on {
            painter.rect_filled(cell, 0.0, Color32::from_gray(0x2c));
            painter.rect_stroke(cell, 0.0, Stroke::new(1.0, Color32::from_gray(0x8a)), egui::StrokeKind::Inside);
        } else if resp.hovered() {
            painter.rect_filled(cell, 0.0, Color32::from_gray(0x22));
        }
        let color = if on { Color32::WHITE } else if resp.hovered() { Color32::from_gray(0xe0) } else { Color32::from_gray(0x9a) };
        // Icon: rectangle in the ratio, at most 18 × 12 pt; "Free" dashed
        let center = cell.center_top() + egui::vec2(0.0, 14.0);
        let q = ratio.map(|q| if landscape { q } else { 1.0 / q });
        let (bw, bh) = match q {
            Some(q) if q >= 1.5 => (18.0, 18.0 / q),
            Some(q) => ((12.0 * q).min(18.0), 12.0_f32.min(18.0 / q.max(1e-3))),
            None => (16.0, 11.0),
        };
        let b = egui::Rect::from_center_size(center, egui::vec2(bw, bh.max(4.0)));
        if ratio.is_some() {
            painter.rect_stroke(b, 0.0, Stroke::new(1.2, color), egui::StrokeKind::Middle);
        } else {
            for (p0, p1) in [(b.left_top(), b.right_top()), (b.right_top(), b.right_bottom()), (b.right_bottom(), b.left_bottom()), (b.left_bottom(), b.left_top())] {
                painter.add(egui::Shape::dashed_line(&[p0, p1], Stroke::new(1.2, color), 2.5, 2.0));
            }
        }
        // Portrait: label the ratio in portrait as well ("3:2" → "2:3")
        let label = match label.split_once(':') {
            Some((a, b)) if !landscape && a != b => format!("{b}:{a}"),
            _ => label.to_string(),
        };
        let g = caps_spaced(painter, &label, egui::FontId::proportional(10.0), color, 0.4);
        painter.galley(egui::pos2(cell.center().x - g.size().x / 2.0, cell.bottom() - 4.0 - g.size().y), g, color);
        if resp.clicked() {
            clicked = Some(i);
        }
    }
    clicked
}

/// Two small icon buttons landscape/portrait (lying and standing rectangle). Returns: toggled?
pub fn orientation_toggle(ui: &mut egui::Ui, landscape: &mut bool) -> bool {
    let (h, w) = (20.0, 24.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(2.0 * w, h), egui::Sense::hover());
    let painter = ui.painter().clone();
    painter.rect_filled(rect, 0.0, PANEL_DARK);
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0, Color32::from_gray(0x4a)), egui::StrokeKind::Inside);
    let mut changed = false;
    for (i, (value, tip)) in [(true, "Querformat (X)"), (false, "Hochformat (X)")].into_iter().enumerate() {
        let seg = egui::Rect::from_min_size(rect.min + egui::vec2(i as f32 * w, 0.0), egui::vec2(w, h));
        let resp = ui.interact(seg, ui.scope_id().with(("lr-orientation", i)), egui::Sense::click());
        let active = *landscape == value;
        if active {
            painter.rect_filled(seg.shrink(2.0), 0.0, Color32::from_gray(0x62));
        } else if resp.hovered() {
            painter.rect_filled(seg.shrink(2.0), 0.0, Color32::from_gray(0x3c));
        }
        let color = if active { Color32::WHITE } else if resp.hovered() { TEXT } else { TEXT_DIM };
        let size = if value { egui::vec2(13.0, 9.0) } else { egui::vec2(9.0, 13.0) };
        painter.rect_stroke(egui::Rect::from_center_size(seg.center(), size), 0.0, Stroke::new(1.2, color), egui::StrokeKind::Middle);
        if resp.clicked() && !active {
            *landscape = value;
            changed = true;
        }
        hover_tip(ui, &resp, tip);
    }
    changed
}



/// Result of [`thumb_grid_fav`].
#[derive(Default)]
pub struct ThumbGridResponse {
    pub clicked: Option<usize>,
    pub hovered: Option<usize>,
    /// Star clicked (toggle favourite)
    pub star: Option<usize>,
    pub visible: Vec<usize>,
    pub hide: Option<usize>,
    /// image area of each visible tile (for marks drawn on top, e.g. the number in the look comparison)
    pub rects: Vec<(usize, egui::Rect)>,
}

/// Like [`thumb_grid`], with a star at the top right of each preview: filled for favourites, otherwise
/// only visible on hover.
pub fn thumb_grid_fav(ui: &mut egui::Ui, id: &str, items: &[(Option<egui::TextureHandle>, &str)], selected: Option<usize>, cols: usize, stars: &[bool]) -> ThumbGridResponse {
    thumb_grid_fav_menu(ui, id, items, selected, cols, stars, false)
}

pub fn thumb_grid_fav_menu(ui: &mut egui::Ui, id: &str, items: &[(Option<egui::TextureHandle>, &str)], selected: Option<usize>, cols: usize, stars: &[bool], hide: bool) -> ThumbGridResponse {
    thumb_grid_fav_available(ui,id,items,selected,cols,stars,hide,&[])
}

/// Offline profiles stay visible but trigger neither selection nor a preview job.
pub fn thumb_grid_fav_available(ui: &mut egui::Ui, id: &str, items: &[(Option<egui::TextureHandle>, &str)], selected: Option<usize>, cols: usize, stars: &[bool], hide: bool, available:&[bool]) -> ThumbGridResponse {
    let tiles: Vec<Tile> = items
        .iter()
        .enumerate()
        .map(|(i, (tex, name))| Tile {
            tex: tex.clone(),
            name,
            sub: "",
            star: Some(stars.get(i).copied().unwrap_or(false)),
            available: available.get(i).copied().unwrap_or(true),
        })
        .collect();
    tile_grid(ui, id, &tiles, selected, cols, hide)
}

/// One tile of [`tile_grid`].
pub struct Tile<'a> {
    pub tex: Option<egui::TextureHandle>,
    pub name: &'a str,
    /// dimmed second line (film, pack, "CUSTOM"); empty: none
    pub sub: &'a str,
    /// favourite star: `None` = the tile has none
    pub star: Option<bool>,
    pub available: bool,
}

/// Tile grid of "Film & Look" (camera looks, Sigma modes, film looks, packs): an image field in the shape of the photo,
/// below it the name (up to two rows) and an optional dimmed second line. No card, no padding;
/// a frame only for the selection (bright) and on hover. The text height follows the content of
/// the whole grid (one row when all names fit into one), so all tiles of a grid have the same size
/// and the height stays the same from frame to frame.
pub fn tile_grid(ui: &mut egui::Ui, id: &str, tiles: &[Tile], selected: Option<usize>, cols: usize, hide: bool) -> ThumbGridResponse {
    const NAME_SIZE: f32 = 10.0;
    const SUB_SIZE: f32 = 9.5;
    const NAME_TOP: f32 = 5.0;
    let w = ui.available_width();
    let gap = 6.0;
    let cols = cols.max(1);
    let cell_w = ((w - gap * (cols as f32 - 1.0)) / cols as f32).floor();
    // image field in the shape of the photo (portrait photos get portrait tiles, no black bars); all tiles show the same photo
    let shape = tiles.iter().find_map(|t| t.tex.as_ref()).map(|t| t.size_vec2()).filter(|s| s.x > 0.0).map_or(2.0 / 3.0, |s| (s.y / s.x).clamp(0.5, 1.5));
    let img_h = (cell_w * shape).round();
    let name_job = |text: &str, color: Color32| {
        let mut job = egui::text::LayoutJob::simple(text.to_string(), egui::FontId::proportional(NAME_SIZE), color, cell_w);
        job.wrap.max_rows = 2;
        job
    };
    let row_h = |size: f32| ui.painter().layout_no_wrap("Ag".into(), egui::FontId::proportional(size), Color32::WHITE).size().y;
    let name_rows = tiles.iter().map(|t| ui.painter().layout_job(name_job(t.name, TEXT)).rows.len()).max().unwrap_or(1).clamp(1, 2);
    let with_sub = tiles.iter().any(|t| !t.sub.is_empty());
    let sub_h = row_h(SUB_SIZE);
    let text_h = NAME_TOP + name_rows as f32 * row_h(NAME_SIZE) + if with_sub { 1.0 + sub_h } else { 0.0 };
    let cell_h = img_h + text_h.ceil();
    // rows a little further apart than the columns, so the text belongs to the image above it
    let row_gap = gap + 4.0;
    let rows = tiles.len().div_ceil(cols);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, rows as f32 * cell_h + (rows.max(1) as f32 - 1.0) * row_gap), egui::Sense::hover());
    let mut out = ThumbGridResponse::default();
    for (i, t) in tiles.iter().enumerate() {
        let available = t.available;
        let (r, c) = (i / cols, i % cols);
        let cell = egui::Rect::from_min_size(rect.min + egui::vec2(c as f32 * (cell_w + gap), r as f32 * (cell_h + row_gap)), egui::vec2(cell_w, cell_h));
        if !ui.is_rect_visible(cell) {
            continue;
        }
        out.visible.push(i);
        let img = egui::Rect::from_min_size(cell.min, egui::vec2(cell_w, img_h));
        out.rects.push((i, img));
        let star_rect = egui::Rect::from_center_size(img.right_top() + egui::vec2(-11.0, 11.0), egui::vec2(18.0, 18.0));
        let star = t.star.map(|_| ui.interact(star_rect, ui.scope_id().with((id, i, "stern")), egui::Sense::click()));
        let resp = ui.interact(cell, ui.scope_id().with((id, i)), egui::Sense::click());
        if hide {
            resp.context_menu(|ui| {
                if ui.button(crate::tr("Ausblenden")).clicked() { out.hide = Some(i); ui.close(); }
            });
        }
        let on = selected == Some(i);
        let star_hovered = star.as_ref().is_some_and(|s| s.hovered());
        let painter = ui.painter();
        painter.rect_filled(img, 0.0, PANEL_DARK);
        if let Some(tex) = &t.tex {
            let s = tex.size_vec2();
            let k = (img.width() / s.x).min(img.height() / s.y);
            painter.image(tex.id(), egui::Rect::from_center_size(img.center(), s * k), egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), if available { Color32::WHITE } else { Color32::from_gray(80) });
        } else {
            painter.text(img.center(), egui::Align2::CENTER_CENTER, if available { "…" } else { crate::tr("Nicht verfügbar") }, egui::FontId::proportional(if available { 12.0 } else { 8.5 }), TEXT_DIM);
        }
        let hot = resp.hovered() || star_hovered;
        if on {
            painter.rect_stroke(img, 0.0, Stroke::new(2.0, Color32::from_gray(0xf0)), egui::StrokeKind::Outside);
        } else if hot {
            painter.rect_stroke(img, 0.0, Stroke::new(1.0, Color32::from_gray(0x80)), egui::StrokeKind::Outside);
        }
        if let Some(fav) = t.star.filter(|&f| f || hot) {
            painter.circle_filled(star_rect.center(), 9.0, Color32::from_black_alpha(150));
            let col = if fav { Color32::from_rgb(0xf0, 0xc8, 0x50) } else if star_hovered { Color32::WHITE } else { Color32::from_gray(0xb0) };
            painter.text(star_rect.center(), egui::Align2::CENTER_CENTER, if fav { "★" } else { "☆" }, egui::FontId::proportional(12.0), col);
        }
        let color = if !available { Color32::from_gray(0x7a) } else if on { Color32::WHITE } else if hot { Color32::from_gray(0xe8) } else { Color32::from_gray(0xb4) };
        let g = painter.layout_job(name_job(t.name, color));
        let name_h = g.size().y;
        painter.galley(egui::pos2(cell.left(), img.bottom() + NAME_TOP), g, color);
        if !t.sub.is_empty() {
            // directly below the name (a one-row name leaves the free row at the bottom of the tile)
            let sub_color = if available { Color32::from_gray(0x80) } else { Color32::from_gray(0x5a) };
            let mut job = egui::text::LayoutJob::simple(t.sub.to_string(), egui::FontId::proportional(SUB_SIZE), sub_color, cell_w);
            job.wrap.max_rows = 1;
            let g = painter.layout_job(job);
            painter.galley(egui::pos2(cell.left(), img.bottom() + NAME_TOP + name_h + 1.0), g, sub_color);
        }
        if star.as_ref().is_some_and(|s| s.clicked()) {
            out.star = Some(i);
        } else if available && resp.clicked() {
            out.clicked = Some(i);
        }
        if hot && available {
            out.hovered = Some(i);
        }
    }
    out
}

/// Toggle button with icon and short label (toolbars): clearly filled when active.
/// Fixed width so nothing jumps. Returns: clicked.
pub fn icon_toggle(ui: &mut egui::Ui, icon: Icon, label: &str, on: bool, width: f32) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 24.0), egui::Sense::click());
    let painter = ui.painter();
    let enabled = ui.is_enabled();
    let (fill, fg) = if on {
        (Color32::from_gray(0x38), Color32::WHITE)
    } else if resp.hovered() && enabled {
        (Color32::from_gray(0x2a), Color32::from_gray(0xe0))
    } else {
        (Color32::from_gray(0x1e), if enabled { TEXT } else { Color32::from_gray(0x5c) })
    };
    painter.rect_filled(rect, 0.0, fill);
    // the brighter fill already shows "on" clearly; only focus gets a frame
    if resp.has_focus() {
        painter.rect_stroke(rect, 0.0, egui::Stroke::new(1.5, ACCENT), egui::StrokeKind::Inside);
    }
    let ir = egui::Rect::from_center_size(egui::pos2(rect.left() + 13.0, rect.center().y), egui::vec2(15.0, 15.0));
    paint_icon(painter, ir, icon, fg);
    let clip = painter.with_clip_rect(rect.shrink(2.0));
    clip.text(egui::pos2(rect.left() + 25.0, rect.center().y), egui::Align2::LEFT_CENTER, crate::tr(label), egui::FontId::proportional(10.5), fg);
    resp.clicked() && enabled
}

/// Icon button without label (toolbar), 26 × 24, filled when active. The hint names
/// the keyboard shortcut from the central table (`shortcuts`), so it is learned while using it.
pub fn icon_button(ui: &mut egui::Ui, icon: Icon, on: bool) -> bool {
    let (tip, shortcut) = match icon {
        Icon::Info => ("Informationen", ""),
        Icon::Sliders => ("Bearbeiten", "tab_edit"),
        Icon::Flag | Icon::CheckCircle => ("Als Auswahl markieren", "pick"),
        Icon::X => ("Als abgelehnt markieren", "reject"),
        Icon::RotCw => ("Nach rechts drehen", "rotate_right"),
        Icon::RotCcw => ("Nach links drehen", "rotate_left"),
        Icon::Aperture => ("Sensor-Ansicht", "sensor_field"),
        Icon::Sun => ("HDR-Anzeige", ""),
        Icon::Warning => ("Clipping-Warnung", "clipping"),
        Icon::Smiley => ("Gesichter anzeigen", ""),
        Icon::Crop => ("Freistellen und Ausrichten", "crop"),
        Icon::ZoomIn => ("Vergrößern", ""),
        Icon::ZoomOut => ("Verkleinern", ""),
        Icon::Image => ("Nachher", ""),
        Icon::SplitH => ("Standardentwicklung | Aktuell", "compare"),
        Icon::Before => ("Standardentwicklung", "before"),
        Icon::Camera => ("Kamera-JPEG", "camera_jpeg"),
        _ => ("", ""),
    };
    let tip = with_shortcut(tip, shortcut);
    icon_button_hint(ui, icon, on, &tip)
}

/// Hint text (translated) with the keyboard shortcut from the central table in brackets, e.g. "Als Auswahl markieren (P)".
pub fn with_shortcut(tip: &str, shortcut: &str) -> String {
    let k = if shortcut.is_empty() { String::new() } else { crate::shortcuts::display(shortcut) };
    match (tip.is_empty(), k.is_empty()) {
        (true, _) => String::new(),
        (false, true) => crate::tr(tip).to_string(),
        (false, false) => format!("{} ({k})", crate::tr(tip)),
    }
}

/// Like [`icon_button`] with its own hint text (appears after a short hover delay).
pub fn icon_button_hint(ui: &mut egui::Ui, icon: Icon, on: bool, tip: &str) -> bool {
    icon_button_sized(ui, icon, on, tip, egui::vec2(26.0, 24.0), 16.0)
}

/// [`icon_button_hint`] with its own size of the field and of the symbol (head rows with small controls).
pub fn icon_button_sized(ui: &mut egui::Ui, icon: Icon, on: bool, tip: &str, size: egui::Vec2, icon_px: f32) -> bool {
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let enabled = ui.is_enabled();
    let painter = ui.painter();
    let (fill, fg) = if on {
        (Color32::from_gray(0x38), Color32::WHITE)
    } else if resp.hovered() && enabled {
        (Color32::from_gray(0x2a), Color32::from_gray(0xe8))
    } else {
        (Color32::TRANSPARENT, if enabled { TEXT } else { Color32::from_gray(0x5c) })
    };
    painter.rect_filled(rect, 0.0, fill);
    if on || resp.has_focus() {
        painter.rect_stroke(rect, 0.0, egui::Stroke::new(if resp.has_focus() { 1.5 } else { 1.0 }, ACCENT), egui::StrokeKind::Inside);
    }
    paint_icon(painter, egui::Rect::from_center_size(rect.center(), egui::vec2(icon_px, icon_px)), icon, fg);
    if !tip.is_empty() && enabled {
        hover_tip(ui, &resp, tip);
    }
    resp.clicked() && enabled
}

/// Height of the fields in the filter bar (search field, groups, selection): one shared grid.
pub const BAR_H: f32 = 28.0;

/// Content of a cell in [`cell_group`].
pub enum Cell<'a> {
    Symbol(Icon, Color32),
    Text(&'a str, f32, Color32),
    /// Flag state ([`paint_flag`]: 1 pick, 0 none, −1 rejected); filled when the cell is active
    Flag(i8),
    /// Color swatch (active: white ring instead of a fill)
    Color(Color32),
}

/// Group of equally wide cells in a frame like the search field (height [`BAR_H`]). Active cells
/// are filled inside, with a gap to the frame, so fill and frame never overlap.
/// Each cell shows its hint after a short hover delay. Returns: clicked cell.
pub fn cell_group(ui: &mut egui::Ui, salt: &str, cells: &[(Cell, bool, &str)], cell_w: f32) -> Option<usize> {
    cell_group_rect(ui, salt, cells, cell_w).0
}

/// Like [`cell_group`], additionally returns the frame (for marks on the group).
pub fn cell_group_rect(ui: &mut egui::Ui, salt: &str, cells: &[(Cell, bool, &str)], cell_w: f32) -> (Option<usize>, egui::Rect) {
    cell_group_sized(ui, salt, cells, cell_w, BAR_H)
}

/// Like [`cell_group_rect`] with its own height (compact rows such as the filmstrip header).
pub fn cell_group_sized(ui: &mut egui::Ui, salt: &str, cells: &[(Cell, bool, &str)], cell_w: f32, h: f32) -> (Option<usize>, egui::Rect) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(cell_w * cells.len() as f32, h), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, GROUP_FILL);
    let mut click = None;
    for (i, (cell, active, tip)) in cells.iter().enumerate() {
        let r = egui::Rect::from_min_size(rect.min + egui::vec2(cell_w * i as f32, 0.0), egui::vec2(cell_w, h));
        let resp = ui.interact(r, ui.id().with((salt, i)), egui::Sense::click());
        let inner = r;
        let is_color_cell = matches!(cell, Cell::Color(_));
        let fill = if *active && !is_color_cell {
            ACTIVE_FILL
        } else if resp.hovered() {
            Color32::from_gray(0x2e)
        } else {
            Color32::TRANSPARENT
        };
        let painter = ui.painter();
        painter.rect_filled(inner, 0.0, fill);
        if *active && !is_color_cell {
            painter.rect_filled(egui::Rect::from_min_max(egui::pos2(inner.left(), inner.bottom() - 2.0), inner.max), 0.0, ACTIVE_MARK);
        }
        match cell {
            Cell::Symbol(icon, c) => paint_icon(painter, egui::Rect::from_center_size(r.center(), egui::vec2(16.0, 16.0)), *icon, if *active { Color32::WHITE } else { *c }),
            Cell::Flag(f) => paint_flag(painter, r.center(), 17.0, *f, *active, if *active { Color32::WHITE } else if resp.hovered() { Color32::from_gray(0xe0) } else { TEXT }),
            Cell::Text(t, size_pt, c) => {
                painter.text(r.center(), egui::Align2::CENTER_CENTER, *t, egui::FontId::proportional(*size_pt), *c);
            }
            Cell::Color(c) => {
                let swatch = egui::Rect::from_center_size(r.center(), egui::vec2(13.0, 13.0));
                painter.rect_filled(swatch, 0.0, *c);
                if *active {
                    painter.rect_stroke(swatch.expand(3.0), 0.0, egui::Stroke::new(1.5, Color32::WHITE), egui::StrokeKind::Outside);
                }
            }
        }
        if !tip.is_empty() {
            hover_tip(ui, &resp, tip);
        }
        if resp.clicked() {
            click = Some(i);
        }
    }
    (click, rect)
}

/// Dropdowns (ComboBox) in the style of the bar fields: height [`BAR_H`], frame like the search field.
/// Applies to the rest of `ui` – so call it in its own `ui.scope`.
pub fn field_style(ui: &mut egui::Ui) {
    ui.spacing_mut().interact_size.y = BAR_H;
    let v = &mut ui.visuals_mut().widgets;
    for w in [&mut v.inactive, &mut v.hovered, &mut v.active, &mut v.open] {
        w.corner_radius = egui::CornerRadius::ZERO;
        w.bg_stroke = egui::Stroke::NONE;
    }
    v.inactive.weak_bg_fill = GROUP_FILL;
    v.inactive.bg_fill = GROUP_FILL;
}

/// Segment of icon buttons (exactly one active). Returns: newly selected index.
pub fn icon_segment(ui: &mut egui::Ui, icons: &[Icon], selected: usize) -> Option<usize> {
    let mut picked = None;
    let start = ui.cursor().min;
    ui.spacing_mut().item_spacing.x = 0.0;
    for (i, icon) in icons.iter().enumerate() {
        if icon_button(ui, *icon, i == selected) && i != selected {
            picked = Some(i);
        }
    }
    let r = egui::Rect::from_min_size(start, egui::vec2(26.0 * icons.len() as f32, 24.0));
    ui.painter().rect_stroke(r, 0.0, egui::Stroke::new(1.0, Color32::from_gray(0x3a)), egui::StrokeKind::Inside);
    ui.spacing_mut().item_spacing.x = 6.0;
    picked
}

/// Hover delay of the tooltips in seconds (common on macOS and Windows: about half a second).
pub const TIP_DELAY: f64 = 0.5;
/// After a tooltip was shown, the next one appears without delay for this long (moving along a toolbar).
const TIP_WARM: f32 = 0.6;

/// Tooltip of a control (all tooltips of the app go through here): appears after [`TIP_DELAY`] of hovering, right away
/// while moving on from another tooltip; below the control (above it when there is no room), so it never covers what
/// the pointer is on; disappears on a click. Keep the text short; a keyboard shortcut goes in brackets, see
/// [`with_shortcut`] and [`tip_key`].
/// Runs `f` (the `ScrollArea::show` call) and, when `placed` (the app set the scroll offset itself this frame),
/// with rubber band and coasting off. egui 0.37 still knows the content height of the previous frame in such a
/// frame; a target behind it counts as over-scrolling, the rubber band builds a huge velocity and the area coasts
/// for seconds to an unrelated place. `decay_time` 0 also clears the stored velocity.
pub fn with_placed_scroll<R>(ui: &mut egui::Ui, placed: bool, f: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let kinetic = ui.spacing().scroll.kinetic;
    if placed {
        ui.spacing_mut().scroll.kinetic.rubber_band = false;
        ui.spacing_mut().scroll.kinetic.decay_time = 0.0;
    }
    let r = f(ui);
    ui.spacing_mut().scroll.kinetic = kinetic;
    r
}

pub fn hover_tip(ui: &egui::Ui, resp: &egui::Response, text: &str) {
    if text.is_empty() {
        return;
    }
    let id = resp.id.with("tipp-start");
    let now = ui.input(|i| i.time);
    let pressed = ui.input(|i| i.pointer.any_down());
    if resp.hovered() && !pressed && !resp.clicked() {
        let warm = egui::Tooltip::seconds_since_last_tooltip(ui.ctx()) < TIP_WARM;
        let start = ui.data_mut(|d| *d.get_temp_mut_or_insert_with(id, || if warm { now - TIP_DELAY } else { now }));
        if start.is_infinite() {
            // hidden after a click
        } else if now - start >= TIP_DELAY {
            let mut tip = egui::Tooltip::always_open(ui.ctx().clone(), resp.layer_id, resp.id, resp.rect);
            tip.popup = tip.popup.align(egui::RectAlign::BOTTOM_START).align_alternatives(&[egui::RectAlign::TOP_START, egui::RectAlign::BOTTOM_END, egui::RectAlign::TOP_END]);
            tip.show(|ui| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                ui.label(egui::RichText::new(crate::tr(text)).size(UI_SIZE - 0.5).color(TEXT))
            });
            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f32(TIP_WARM));
        } else {
            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(TIP_DELAY - (now - start) + 0.01));
        }
    } else if pressed && resp.hovered() {
        // clicked: hidden until the pointer leaves the control
        ui.data_mut(|d| d.insert_temp(id, f64::INFINITY));
    } else if !resp.hovered() {
        ui.data_mut(|d| d.remove_temp::<f64>(id));
    }
}

/// [`hover_tip`] with the keyboard shortcut from the central table ([`crate::shortcuts`]), e.g. "Markieren (P)".
pub fn tip_key(ui: &egui::Ui, resp: &egui::Response, text: &str, shortcut: &str) {
    hover_tip(ui, resp, &with_shortcut(text, shortcut));
}

/// Extension for responses: `ui.add(…).tip(ui, "…")`.
pub trait Tip {
    fn tip(self, ui: &egui::Ui, text: &str) -> Self;
    fn tip_key(self, ui: &egui::Ui, text: &str, shortcut: &str) -> Self;
}

impl Tip for egui::Response {
    fn tip(self, ui: &egui::Ui, text: &str) -> Self {
        hover_tip(ui, &self, text);
        self
    }
    fn tip_key(self, ui: &egui::Ui, text: &str, shortcut: &str) -> Self {
        tip_key(ui, &self, text, shortcut);
        self
    }
}

/// Entry of a navigation or work bar (see [`bar`]).
pub struct BarEntry<'a> {
    pub text: &'a str,
    pub active: bool,
    pub enabled: bool,
    pub hover_tip: &'a str,
}

/// Segmented bar (modules, views): light fill and underline for the active entry, the
/// others dimmed. Returns: index of the clicked entry. Placed right-aligned when it sits in
/// a right-to-left layout.
pub fn bar(ui: &mut egui::Ui, entries: &[BarEntry]) -> Option<usize> {
    bar_with(ui, entries, GOLD)
}

/// Like [`bar`] with a selectable underline color (darkroom: safelight).
pub fn bar_with(ui: &mut egui::Ui, entries: &[BarEntry], accent: Color32) -> Option<usize> {
    bar_pad(ui, entries, accent, 12.0)
}

/// Like [`bar_with`] with a selectable inner padding of each entry (narrow bars with many entries).
pub fn bar_pad(ui: &mut egui::Ui, entries: &[BarEntry], accent: Color32, pad: f32) -> Option<usize> {
    let PAD: f32 = pad;
    let font = egui::TextStyle::Button.resolve(ui.style());
    let galleys: Vec<_> = entries.iter().map(|e| ui.painter().layout_no_wrap(crate::tr(e.text).to_string(), font.clone(), Color32::WHITE)).collect();
    let total: f32 = galleys.iter().map(|g| g.size().x + 2.0 * PAD).sum::<f32>() + 2.0 * (entries.len().saturating_sub(1)) as f32;
    let mut clicked = None;
    ui.allocate_ui_with_layout(egui::vec2(total, 24.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for (i, (e, g)) in entries.iter().zip(&galleys).enumerate() {
            let sense = if e.enabled { egui::Sense::click() } else { egui::Sense::hover() };
            let (r, resp) = ui.allocate_exact_size(egui::vec2(g.size().x + 2.0 * PAD, 24.0), sense);
            let hover = resp.hovered() && e.enabled;
            let p = ui.painter();
            if e.active {
                p.rect_filled(r, 0.0, Color32::from_gray(0x2e));
                p.rect_filled(egui::Rect::from_min_max(egui::pos2(r.left() + 4.0, r.bottom() - 2.0), egui::pos2(r.right() - 4.0, r.bottom())), 0.0, accent);
            } else if hover {
                p.rect_filled(r, 0.0, Color32::from_gray(0x22));
            }
            let color = if e.active { Color32::WHITE } else if !e.enabled { Color32::from_gray(0x78) } else if hover { TEXT } else { TEXT_DIM };
            let g2 = p.layout_no_wrap(crate::tr(e.text).to_string(), font.clone(), color);
            p.galley(egui::pos2(r.center().x - g2.size().x / 2.0, r.center().y - g2.size().y / 2.0 - 1.0), g2, color);
            if !e.hover_tip.is_empty() {
                hover_tip(ui, &resp, e.hover_tip);
            }
            if resp.clicked() {
                clicked = Some(i);
            }
        }
    });
    clicked
}

/// Height of a [`steps`] control: label row and track as with [`slider`], plus one small line for the step names.
pub const STEPS_H: f32 = 44.0;

/// Steps control for ordered choices with few steps (in the style of SIGMA Photo Pro): name at the top left as with
/// [`slider`], the name of the chosen step at the top right; below a line across the width with one dot per step
/// (chosen dot filled and light, the others hollow) and the step names small and dimmed under their dots.
/// - Click or drag on the line jumps to the nearest step
/// - Double-click on name or line: `default`
/// - Mouse over the row + ↑/↓ moves one step
///
/// The help "?" behind the name comes from [`crate::help::for_slider`] (`hint` tells controls with the same name apart).
pub fn steps<T: PartialEq + Clone>(ui: &mut egui::Ui, label: &str, value: &mut T, options: &[(T, &str)], default: T, hint: &str) -> bool {
    let n = options.len();
    let (row, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), STEPS_H), egui::Sense::hover());
    if n == 0 {
        return false;
    }
    let before = value.clone();
    let enabled = ui.is_enabled();
    let pad = 7.0;
    let label_rect = egui::Rect::from_min_size(row.min, egui::vec2(row.width() * 0.5, 17.0));
    let value_rect = egui::Rect::from_min_max(egui::pos2(row.center().x, row.top()), egui::pos2(row.right(), row.top() + 17.0));
    let line_y = row.top() + 17.0 + 7.0;
    let track_rect = egui::Rect::from_min_max(egui::pos2(row.left() + pad, row.top() + 17.0), egui::pos2(row.right() - pad, row.bottom()));
    let id = ui.scope_id().with(("lr-steps", label));
    let name = ui.interact(label_rect, id.with("name"), egui::Sense::click());
    let track = ui.interact(track_rect.expand2(egui::vec2(pad, 0.0)), id.with("track"), egui::Sense::click_and_drag());
    let x_of = |i: usize| if n == 1 { track_rect.center().x } else { track_rect.left() + track_rect.width() * i as f32 / (n - 1) as f32 };
    let nearest = |x: f32| (0..n).min_by(|a, b| (x_of(*a) - x).abs().total_cmp(&(x_of(*b) - x).abs())).unwrap_or(0);

    if enabled {
        if name.double_clicked() || track.double_clicked() {
            crate::history::reset_gesture();
            *value = default.clone();
        } else if track.clicked() || track.drag_started() || track.dragged() {
            if let Some(p) = track.interact_pointer_pos() {
                *value = options[nearest(p.x)].0.clone();
            }
        }
    }
    let hot = enabled && (ui.rect_contains_pointer(row) || track.dragged());
    if hot {
        let (up, down) = ui.input(|i| (i.key_pressed(egui::Key::ArrowUp), i.key_pressed(egui::Key::ArrowDown)));
        if let Some(i) = options.iter().position(|(v, _)| v == value) {
            if up && i + 1 < n {
                *value = options[i + 1].0.clone();
            }
            if down && i > 0 {
                *value = options[i - 1].0.clone();
            }
        }
    }
    let sel = options.iter().position(|(v, _)| v == value);
    let def = options.iter().position(|(v, _)| *v == default);
    let changed = *value != default;
    let painter = ui.painter();

    // Name and chosen step, coloured as with the sliders
    let name_color = if !enabled {
        Color32::from_gray(0x5c)
    } else if hot {
        Color32::WHITE
    } else if changed {
        Color32::from_gray(0xe0)
    } else {
        Color32::from_gray(0xa4)
    };
    let name_text = label_caps(painter, label_rect.left_center(), label, name_color);
    if let Some(i) = sel {
        let c = if !enabled { Color32::from_gray(0x5c) } else if changed { Color32::WHITE } else { TEXT_DIM };
        painter.text(value_rect.right_center() - egui::vec2(13.0, 0.0), egui::Align2::RIGHT_CENTER, crate::tr(options[i].1), egui::FontId::proportional(UI_SIZE), c);
    }

    // Line; the stretch from the default to the chosen step slightly brighter (like the slider fill)
    let dim = if enabled { 1.0 } else { 0.55 };
    let rail = if hot { Color32::from_gray(0x5a) } else { Color32::from_gray(0x46) };
    painter.line_segment([egui::pos2(x_of(0), line_y), egui::pos2(x_of(n - 1), line_y)], Stroke::new(1.0, rail.gamma_multiply(dim)));
    if let (Some(a), Some(b)) = (def, sel) {
        if a != b {
            let fill = if enabled { if hot { Color32::from_gray(0xe0) } else { Color32::from_gray(0xb0) } } else { Color32::from_gray(0x5c) };
            painter.line_segment([egui::pos2(x_of(a.min(b)), line_y), egui::pos2(x_of(a.max(b)), line_y)], Stroke::new(2.0, fill));
        }
    }
    let pointer_step = track.hover_pos().filter(|_| enabled).map(|p| nearest(p.x));
    for (i, (_, text)) in options.iter().enumerate() {
        let c = egui::pos2(x_of(i), line_y);
        let on = sel == Some(i);
        if on {
            let knob = if !enabled { Color32::from_gray(0x66) } else if hot { Color32::WHITE } else { Color32::from_gray(0xd8) };
            painter.circle_filled(c, 5.5, Color32::from_black_alpha(140));
            painter.circle_filled(c, 4.5, knob);
        } else {
            let ring = if pointer_step == Some(i) { Color32::from_gray(0xd0) } else { Color32::from_gray(0x80) }.gamma_multiply(dim);
            painter.circle_filled(c, 3.5, PANEL);
            painter.circle_stroke(c, 3.5, Stroke::new(1.0, ring));
        }
        // Step names: small and dimmed; the outer ones aligned to the edges so they stay inside the panel
        let tc = if on && enabled { Color32::from_gray(0xc4) } else { Color32::from_gray(0x86).gamma_multiply(dim) };
        let (align, x) = if n > 1 && i == 0 {
            (egui::Align2::LEFT_TOP, row.left() + 2.0)
        } else if n > 1 && i == n - 1 {
            (egui::Align2::RIGHT_TOP, row.right() - 6.0)
        } else {
            (egui::Align2::CENTER_TOP, c.x)
        };
        painter.text(egui::pos2(x, line_y + 8.0), align, crate::tr(text), egui::FontId::proportional(9.5), tc);
    }
    name_hover(ui, name, name_text, id, hot, label, hint, "Klicken: Stufe wählen · Doppelklick: zurücksetzen · ↑/↓ über dem Regler");
    *value != before
}

/// Name above a control, as with the sliders (capitals, left). Returns: row, text rect, id for the help "?".
fn label_above(ui: &mut egui::Ui, label: &str) -> (egui::Rect, egui::Rect, egui::Id) {
    let (row, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 17.0), egui::Sense::hover());
    let color = if ui.is_enabled() { Color32::from_gray(0xa4) } else { Color32::from_gray(0x5c) };
    let text = label_caps(ui.painter(), row.left_center(), label, color);
    (row, text, ui.scope_id().with(("lr-choice", label)))
}

/// Either-or choice (Color | Black & White, Off | Profile | Manual …): name above as with the sliders, segments
/// across the full width below. `hint` selects the help card as with the sliders (may be empty); the "?" appears
/// behind the name while the control is hovered.
pub fn choice<T: PartialEq + Clone>(ui: &mut egui::Ui, label: &str, value: &mut T, options: &[(T, &str)], hint: &str) -> bool {
    let (row, text, id) = label_above(ui, label);
    let (changed, seg) = segmented_full_rect(ui, value, options);
    let hot = ui.is_enabled() && ui.rect_contains_pointer(row.union(seg));
    if let Some(card) = crate::help::for_slider(label, hint) {
        crate::help::question_mark(ui, egui::pos2(text.right() + 10.0, text.center().y), id.with("hilfe"), hot, label, hint, card, "");
    }
    ui.add_space(4.0);
    changed
}

/// Like [`choice`], with a short state at the right end of the name row (e.g. "×4 · 1356 × 904"), in dim text.
pub fn choice_state<T: PartialEq + Clone>(ui: &mut egui::Ui, label: &str, state: &str, value: &mut T, options: &[(T, &str)], hint: &str) -> bool {
    let (row, text, id) = label_above(ui, label);
    if !state.is_empty() {
        ui.painter().text(row.right_center(), egui::Align2::RIGHT_CENTER, state, egui::TextStyle::Small.resolve(ui.style()), TEXT_DIM);
    }
    let (changed, seg) = segmented_full_rect(ui, value, options);
    let hot = ui.is_enabled() && ui.rect_contains_pointer(row.union(seg));
    if let Some(card) = crate::help::for_slider(label, hint) {
        crate::help::question_mark(ui, egui::pos2(text.right() + 10.0, text.center().y), id.with("hilfe"), hot, label, hint, card, "");
    }
    ui.add_space(4.0);
    changed
}

/// Like [`choice`], with a tooltip per segment (`tips`, German texts) for choices whose names need a word of
/// explanation.
pub fn choice_tips<T: PartialEq + Clone>(ui: &mut egui::Ui, label: &str, value: &mut T, options: &[(T, &str)], tips: &[&str], hint: &str) -> bool {
    let (row, text, id) = label_above(ui, label);
    let (changed, seg) = segmented_full_tips(ui, value, options, tips);
    let hot = ui.is_enabled() && ui.rect_contains_pointer(row.union(seg));
    if let Some(card) = crate::help::for_slider(label, hint) {
        crate::help::question_mark(ui, egui::pos2(text.right() + 10.0, text.center().y), id.with("hilfe"), hot, label, hint, card, "");
    }
    ui.add_space(4.0);
    changed
}

/// Like [`segmented`], but across the full available width (the segments share the spare width).
pub fn segmented_full<T: PartialEq + Clone>(ui: &mut egui::Ui, value: &mut T, options: &[(T, &str)]) -> bool {
    segmented_full_rect(ui, value, options).0
}

fn segmented_full_rect<T: PartialEq + Clone>(ui: &mut egui::Ui, value: &mut T, options: &[(T, &str)]) -> (bool, egui::Rect) {
    segmented_full_tips(ui, value, options, &[])
}

/// [`segmented_full_rect`] with a tooltip per segment (`tips[i]`, German text, empty = none).
fn segmented_full_tips<T: PartialEq + Clone>(ui: &mut egui::Ui, value: &mut T, options: &[(T, &str)], tips: &[&str]) -> (bool, egui::Rect) {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let h = 20.0;
    let w = ui.available_width();
    let natural: Vec<f32> = options.iter().map(|(_, l)| ui.painter().layout_no_wrap(crate::tr(l).to_string(), font.clone(), TEXT).size().x + 8.0).collect();
    let sum: f32 = natural.iter().sum();
    let widths: Vec<f32> = if sum >= w {
        natural.iter().map(|x| x * w / sum.max(1.0)).collect()
    } else {
        let extra = (w - sum) / options.len().max(1) as f32;
        natural.iter().map(|x| x + extra).collect()
    };
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
    let painter = ui.painter().clone();
    painter.rect_filled(rect, 0.0, PANEL_DARK);
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0, Color32::from_gray(0x4a)), egui::StrokeKind::Inside);
    let base = ui.scope_id().with(("lr-segmented-full", options.first().map(|o| o.1).unwrap_or_default()));
    let mut x = rect.left();
    let mut changed = false;
    for (i, ((v, label), sw)) in options.iter().zip(&widths).enumerate() {
        let seg = egui::Rect::from_min_size(egui::pos2(x, rect.top()), egui::vec2(*sw, h));
        x += sw;
        let resp = ui.interact(seg, base.with(i), egui::Sense::click());
        if let Some(t) = tips.get(i).filter(|t| !t.is_empty()) {
            hover_tip(ui, &resp, &crate::tr(t));
        }
        if resp.clicked() && *value != *v {
            *value = v.clone();
            changed = true;
        }
        let active = *value == *v;
        if active {
            painter.rect_filled(seg.shrink(2.0), 0.0, Color32::from_gray(0x62));
        } else if resp.hovered() {
            painter.rect_filled(seg.shrink(2.0), 0.0, Color32::from_gray(0x3c));
        }
        if i > 0 && !active {
            painter.vline(seg.left(), seg.y_range().shrink(4.0), Stroke::new(1.0, Color32::from_gray(0x40)));
        }
        let color = if active { Color32::WHITE } else if resp.hovered() { TEXT } else { TEXT_DIM };
        let galley = painter.layout_no_wrap(crate::tr(label).to_string(), font.clone(), color);
        painter.with_clip_rect(seg.shrink(1.0)).galley(seg.center() - galley.size() / 2.0, galley, color);
    }
    (changed, rect)
}

/// Icon and name in one line (for [`icon_select`]); `PLACEHOLDER` takes the text colour of the widget.
fn icon_text(icon: Icon, text: &str) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let fmt = |font_id: egui::FontId| egui::TextFormat { font_id, color: Color32::PLACEHOLDER, valign: egui::Align::Center, ..Default::default() };
    job.append(&icon.glyph().to_string(), 0.0, fmt(icon_font(13.0)));
    job.append(text, 8.0, fmt(egui::FontId::proportional(UI_SIZE)));
    job
}

/// Compact choice with icons: name above as with the sliders, below a drop-down list (icon and name per entry,
/// the chosen one visible with its icon) and on the right an icon button (e.g. picker).
/// `shown`: what the list shows when no entry is chosen (e.g. own white balance).
/// Returns: (chosen entry, icon button clicked).
#[allow(clippy::too_many_arguments)]
pub fn icon_select(
    ui: &mut egui::Ui,
    id: &str,
    label: &str,
    items: &[(Icon, &str)],
    selected: Option<usize>,
    shown: (Icon, &str),
    button: Icon,
    button_on: bool,
    button_tip: &str,
) -> (Option<usize>, bool) {
    label_above(ui, label);
    let mut picked = None;
    let mut pressed = false;
    let w = ui.available_width();
    ui.allocate_ui_with_layout(egui::vec2(w, 24.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let (icon, text) = selected.and_then(|i| items.get(i).copied()).unwrap_or(shown);
        let combo_w = (w - 26.0 - 6.0).max(60.0);
        egui::ComboBox::from_id_salt(("lr-icon-select", id))
            .selected_text(icon_text(icon, crate::tr(text)))
            .width(combo_w)
            .height(400.0)
            .show_ui(ui, |ui| {
                for (i, (icon, text)) in items.iter().enumerate() {
                    if ui.selectable_label(selected == Some(i), icon_text(*icon, crate::tr(text))).clicked() {
                        picked = Some(i);
                    }
                }
            });
        pressed = icon_button_hint(ui, button, button_on, button_tip);
    });
    ui.add_space(4.0);
    (picked, pressed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dragging the histogram to the right makes the image brighter in all three areas (positive slider value).
    #[test]
    fn histogram_drag_right_is_brighter() {
        for r in [HistRegion::Shadows, HistRegion::Exposure, HistRegion::Highlights] {
            assert!(r.slider_delta(0.1) > 0.0, "{r:?} right");
            assert!(r.slider_delta(-0.1) < 0.0, "{r:?} left");
        }
    }

    /// Cursor egui asks for when the pointer rests over the widget drawn by `add` (a few passes: hover needs the rect of the previous one).
    fn cursor_over(add: impl Fn(&mut egui::Ui) -> egui::Response) -> egui::CursorIcon {
        let ctx = egui::Context::default();
        let mut pos = egui::pos2(-100.0, -100.0);
        let mut icon = egui::CursorIcon::Default;
        for _ in 0..3 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 200.0))),
                events: vec![egui::Event::PointerMoved(pos)],
                ..Default::default()
            };
            let out = crate::ui_test::run(&ctx, input, |ui| {
                pos = add(ui).rect.center();
            });
            icon = out.platform_output.cursor_icon;
        }
        icon
    }

    #[test]
    fn bounded_section_clips_an_oversized_child() {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 200.0))),
            ..Default::default()
        };
        let mut extent = 0.0;
        let output = crate::ui_test::run(&ctx, input, |ui| {
            ui.allocate_ui(egui::vec2(180.0, 100.0), |ui| {
                ui.set_width(180.0);
                let start = ui.cursor().left();
                bounded(ui, |ui| {
                    ui.add_sized([500.0, 20.0], egui::Label::new("x".repeat(200)));
                });
                extent = ui.min_rect().right() - start;
            });
        });
        assert!(extent <= 180.1, "parent expanded to {extent}");
        assert!(output.shapes.iter().all(|shape| shape.clip_rect.right() <= 180.1), "drawing escapes the section");
    }

    #[test]
    fn standard_dialog_bounds_long_content_in_all_languages() {
        for language in crate::menu::i18n::Language::ALL {
            let _guard = crate::menu::i18n::locale::TestLanguage::set(language);
            let ctx = egui::Context::default();
            let mut clipped_width = f32::INFINITY;
            for frame in 0..2 {
                let input = egui::RawInput { time: Some(frame as f64), screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 700.0))), ..Default::default() };
                let _ = crate::ui_test::run(&ctx, input, |_ui| {
                    crate::dialog::show(&ctx, "long-content", "Test", 300.0, |ui| {
                        clipped_width = ui.clip_rect().width();
                        ui.add_sized([700.0, 20.0], egui::Label::new("ä長".repeat(100)));
                    }, crate::dialog::Buttons::confirm(crate::tr("OK"), true));
                });
            }
            assert!(clipped_width <= 300.1, "dialog content too wide in {language:?}: {clipped_width}");
        }
    }

    #[test]
    fn bounded_section_draws_below_the_first_screenful() {
        let ctx = egui::Context::default();
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(300.0, 400.0))), ..Default::default() };
        let mut lowest_clip = 0.0f32;
        let _ = crate::ui_test::run(&ctx, input, |ui| {
            // a page taller than the window, as in a scroll area: content below 100 must not be clipped away
            ui.allocate_ui(egui::vec2(300.0, 100.0), |ui| {
                bounded(ui, |ui| {
                    ui.add_space(250.0);
                    lowest_clip = ui.clip_rect().bottom();
                    ui.label("unten");
                });
            });
        });
        assert!(lowest_clip >= 300.0, "content below the first screenful is clipped at {lowest_clip}");
    }

    #[test]
    fn bounded_section_keeps_controls_clickable() {
        let ctx = egui::Context::default();
        let mut clicked = false;
        for events in [
            vec![egui::Event::PointerMoved(egui::pos2(30.0, 10.0))],
            vec![egui::Event::PointerButton { pos: egui::pos2(30.0, 10.0), button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE }],
            vec![egui::Event::PointerButton { pos: egui::pos2(30.0, 10.0), button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE }],
        ] {
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(300.0, 200.0))), events, ..Default::default() };
            let _ = crate::ui_test::run(&ctx, input, |ui| {
                bounded(ui, |ui| { clicked |= ui.button("OK").clicked(); });
            });
        }
        assert!(clicked);
    }

    /// The sliders of the Masks tray (slider_plain) reset on a double-click on the name or the track, as in Edit.
    #[test]
    fn plain_slider_resets_on_double_click() {
        for at in [egui::pos2(20.0, 10.0), egui::pos2(150.0, 28.0)] {
            let ctx = egui::Context::default();
            let mut value = 0.7f32;
            let mut changed = false;
            let press = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
            for (i, events) in [vec![egui::Event::PointerMoved(at)], vec![press(true)], vec![press(false)], vec![press(true)], vec![press(false)], vec![]].into_iter().enumerate() {
                let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(300.0, 100.0))), time: Some(i as f64 * 0.05), events, ..Default::default() };
                let _ = crate::ui_test::run(&ctx, input, |ui| {
                    changed |= slider_plain_f32(ui, "Belichtung", &mut value, -3.0..=3.0, 0.0, 0.01);
                });
            }
            assert!(changed, "no change at {at:?}");
            assert_eq!(value, 0.0, "double-click at {at:?} did not reset");
        }
    }

    #[test]
    fn long_metadata_stays_within_its_row() {
        let ctx = egui::Context::default();
        let long = "Langer-Dateiname-".repeat(20);
        for width in [120.0, 180.0, 260.0, 400.0] {
            let mut right = 0.0;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 200.0))),
                ..Default::default()
            };
            let output = crate::ui_test::run(&ctx, input, |ui| {
                ui.allocate_ui(egui::vec2(width, 100.0), |ui| {
                    ui.set_width(width);
                    let start = ui.cursor().left();
                    let response = kv_row(ui, &long, &long, true);
                    let path = format!("/Fotos/{}/bild.jpg", "ä".repeat(200));
                    let shortened = middle_ellipsis(ui, &path, 100.0);
                    assert!(shortened.starts_with('/') && shortened.ends_with(".jpg") && shortened.contains('…'));
                    right = response.rect.right() - start;
                });
            });
            assert!(right <= width + 0.1, "long text exceeds {width}: {right}");
            drop(output);
        }
    }

    /// Labels right-aligned in one column (as wide as the longest label), values left-aligned on one
    /// axis; values in parts wrap at their separators, paths are shortened at the start.
    #[test]
    fn metadata_rows_share_one_axis() {
        let ctx = egui::Context::default();
        let width = 300.0;
        let mut lefts: Vec<f32> = Vec::new();
        let mut keys_right: Vec<f32> = Vec::new();
        let mut heights: Vec<f32> = Vec::new();
        let mut path_text = String::new();
        for _pass in 0..2 {
            lefts.clear();
            keys_right.clear();
            heights.clear();
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 400.0))), ..Default::default() };
            let _ = crate::ui_test::run(&ctx, input, |ui| {
                ui.allocate_ui(egui::vec2(width, 300.0), |ui| {
                    ui.set_width(width);
                    let start = ui.cursor().left();
                    for (k, v) in [("Ort", "kurz"), ("Belichtung", "1/50 s · f/2,8 · ISO 400 · 30 mm (45 mm KB)"), ("Ordner", "/Users/x/Bilder/2024/Urlaub/Rom/Tag 1")] {
                        let top = ui.cursor().top();
                        let r = kv_row(ui, k, v, false);
                        lefts.push(r.rect.left() - start);
                        heights.push(ui.cursor().top() - top);
                        assert!(r.rect.right() - start <= width + 0.1, "{k} runs out: {}", r.rect.right() - start);
                        let col = kv_key_column(ui, 0.0, width);
                        keys_right.push(col);
                    }
                    let path = "/Users/x/Bilder/2024/Urlaub/Rom/Tag 1/sehr/lang/noch/länger";
                    path_text = start_ellipsis(ui, path, 120.0);
                });
            });
        }
        // the second frame knows the longest label: every value starts at the same x
        assert!(lefts.windows(2).all(|w| (w[0] - w[1]).abs() < 0.5), "{lefts:?}");
        assert!(lefts[0] > 0.0 && lefts[0] <= width * KV_KEY_SHARE + KV_GAP + 0.5);
        // the exposure wraps (two rows) instead of being cut, the short rows keep one line
        assert!(heights[1] > heights[0] + 5.0, "{heights:?}");
        assert!(path_text.starts_with('…') && path_text.ends_with("länger"), "{path_text}");
    }

    /// A scroll area keeps a band free for its bar – content (rows, highlights, numbers on the right)
    /// never reaches the bar.
    #[test]
    fn content_keeps_clear_of_the_scroll_bar() {
        let ctx = egui::Context::default();
        apply_theme(&ctx);
        let mut inner_right = 0.0;
        let mut outer_right = 0.0;
        for _pass in 0..3 {
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 200.0))), ..Default::default() };
            let _ = crate::ui_test::run(&ctx, input, |ui| {
                outer_right = ui.max_rect().right();
                egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                    for i in 0..40 {
                        ui.label(format!("Zeile {i}"));
                    }
                    inner_right = ui.max_rect().right();
                });
            });
        }
        assert!(outer_right - inner_right >= SCROLL_BAND - 0.5, "content up to {inner_right}, bar area ends at {outer_right}");
    }

    #[test]
    fn clickable_text_shows_arrow_not_text_cursor() {
        // cause: a clickable label is selectable by default and then shows the I-beam
        let plain = cursor_over(|ui| ui.add(egui::Label::new("Anna").sense(egui::Sense::click())));
        assert_eq!(plain, egui::CursorIcon::Text);
        // fix: arrow pointer
        let link = cursor_over(|ui| link_text(ui, egui::RichText::new("Anna"), TEXT));
        assert_eq!(link, egui::CursorIcon::Default);
    }

    /// One slider drawn headless, frame by frame, with the pointer events of each frame.
    struct SliderProbe {
        ctx: egui::Context,
        row: egui::Rect,
    }

    impl SliderProbe {
        fn new() -> Self {
            Self { ctx: egui::Context::default(), row: egui::Rect::NOTHING }
        }

        /// Draws `value` with `range` (default `default`, step `step`); returns what the slider reported.
        fn frame(&mut self, events: Vec<egui::Event>, value: &mut f64, range: std::ops::RangeInclusive<f64>, default: f64, step: f64) -> bool {
            let input = egui::RawInput { events, screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0))), ..Default::default() };
            let mut changed = false;
            let mut row = egui::Rect::NOTHING;
            let _ = self.ctx.run_ui(input, |ui| {
                let top = ui.cursor().min;
                row = egui::Rect::from_min_size(top + egui::vec2(0.0, SLIDER_AIR / 2.0), egui::vec2(ui.available_width(), 31.0));
                changed = slider_gradient(ui, "Testregler", value, range.clone(), default, step, "", GRADIENT_TEMP);
            });
            self.row = row;
            changed
        }

        /// A point on the track at fraction `f` (0 … 1).
        fn track(&self, f: f32) -> egui::Pos2 {
            egui::pos2(self.row.left() + 7.0 + (self.row.width() - 14.0) * f, self.row.top() + 24.0)
        }

        fn name(&self) -> egui::Pos2 {
            self.row.left_top() + egui::vec2(10.0, 8.0)
        }
    }

    fn press(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }
    }

    /// Regression: a value outside the range (as-shot 50000 K on a narrower slider, tint −200) was clamped while the
    /// slider was only drawn, and the slider reported that as a change (the white balance turned "custom").
    #[test]
    fn drawing_a_value_outside_the_range_changes_nothing() {
        for (start, range, default, step) in [(50000.0, 2000.0..=12000.0, 5000.0, 50.0), (-200.0, -150.0..=150.0, 0.0, 1.0), (1500.0, 2000.0..=50000.0, 5000.0, 50.0)] {
            let mut p = SliderProbe::new();
            let mut v = start;
            for _ in 0..3 {
                assert!(!p.frame(vec![], &mut v, range.clone(), default, step), "no change reported for {start}");
            }
            // hovering the track and the value field neither
            let over = vec![egui::Event::PointerMoved(p.track(0.5))];
            assert!(!p.frame(over, &mut v, range.clone(), default, step));
            let field = egui::Event::PointerMoved(egui::pos2(p.row.right() - 20.0, p.row.top() + 8.0));
            assert!(!p.frame(vec![field], &mut v, range.clone(), default, step));
            assert!(!p.frame(vec![], &mut v, range.clone(), default, step));
            assert_eq!(v, start, "the value outside the range is kept as it is");
        }
    }

    /// Values inside the range: drawing reports nothing, a click on the track sets the value there.
    #[test]
    fn a_click_on_the_track_still_changes_the_value() {
        let mut p = SliderProbe::new();
        let mut v = 0.0;
        assert!(!p.frame(vec![], &mut v, -100.0..=100.0, 0.0, 1.0));
        let at = p.track(0.75);
        p.frame(vec![egui::Event::PointerMoved(at)], &mut v, -100.0..=100.0, 0.0, 1.0);
        let a = p.frame(vec![press(at, true)], &mut v, -100.0..=100.0, 0.0, 1.0);
        let b = p.frame(vec![press(at, false)], &mut v, -100.0..=100.0, 0.0, 1.0);
        assert!(a || b, "the click is reported");
        assert!((v - 50.0).abs() <= 1.0, "value at 3/4 of the track: {v}");
        // clicking a slider whose value lies outside: the value jumps into the range, reported as a change
        let mut p = SliderProbe::new();
        let mut v = 50000.0;
        p.frame(vec![], &mut v, 2000.0..=12000.0, 5000.0, 50.0);
        let at = p.track(0.5);
        p.frame(vec![egui::Event::PointerMoved(at)], &mut v, 2000.0..=12000.0, 5000.0, 50.0);
        let a = p.frame(vec![press(at, true)], &mut v, 2000.0..=12000.0, 5000.0, 50.0);
        let b = p.frame(vec![press(at, false)], &mut v, 2000.0..=12000.0, 5000.0, 50.0);
        assert!(a || b);
        assert!((v - 7000.0).abs() <= 50.0, "{v}");
    }

    /// A double-click on the name resets to the default value, also from outside the range.
    #[test]
    fn a_double_click_resets_the_slider() {
        for start in [37.0, 250.0] {
            let mut p = SliderProbe::new();
            let mut v = start;
            p.frame(vec![], &mut v, -100.0..=100.0, 0.0, 1.0);
            let at = p.name();
            let mut reported = false;
            reported |= p.frame(vec![egui::Event::PointerMoved(at)], &mut v, -100.0..=100.0, 0.0, 1.0);
            for _ in 0..2 {
                reported |= p.frame(vec![press(at, true)], &mut v, -100.0..=100.0, 0.0, 1.0);
                reported |= p.frame(vec![press(at, false)], &mut v, -100.0..=100.0, 0.0, 1.0);
            }
            assert!(reported, "the reset is reported ({start})");
            assert_eq!(v, 0.0);
        }
    }

    /// ↑ over the slider steps the value (and clamps it into the range on the way).
    #[test]
    fn arrow_keys_over_the_slider_step_the_value() {
        let key = |k| egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() };
        let mut p = SliderProbe::new();
        let mut v = 10.0;
        p.frame(vec![], &mut v, -100.0..=100.0, 0.0, 1.0);
        let at = p.track(0.2);
        p.frame(vec![egui::Event::PointerMoved(at)], &mut v, -100.0..=100.0, 0.0, 1.0);
        assert!(p.frame(vec![key(egui::Key::ArrowUp)], &mut v, -100.0..=100.0, 0.0, 1.0));
        assert_eq!(v, 11.0);
        let mut v = 130.0;
        assert!(p.frame(vec![key(egui::Key::ArrowDown)], &mut v, -100.0..=100.0, 0.0, 1.0));
        assert_eq!(v, 100.0);
    }
}

/// Vector symbols in the line style of the [`Icon`] glyphs (for symbols the icon font lacks).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DrawnIcon {
    /// Darkroom: enlarger (lamp house on a column, lens, light cone onto the baseboard)
    Enlarger,
    /// Comparison in fields: two frames side by side
    TwoFields,
}

/// Paints `icon` into `rect` (square, thin lines like Phosphor Light).
pub fn paint_drawn_icon(painter: &egui::Painter, rect: egui::Rect, icon: DrawnIcon, color: Color32) {
    let k = rect.width().min(rect.height()) / 16.0;
    let o = rect.center() - egui::vec2(8.0, 8.0) * k;
    let p = |x: f32, y: f32| o + egui::vec2(x, y) * k;
    let line = egui::Stroke::new(1.1 * k.max(1.0), color);
    match icon {
        DrawnIcon::Enlarger => {
            // baseboard with the paper (easel) on it, column on the right
            painter.line_segment([p(1.0, 14.5), p(15.0, 14.5)], line);
            painter.rect_stroke(egui::Rect::from_min_max(p(2.0, 12.0), p(10.0, 14.5)), 0.5 * k, line, egui::StrokeKind::Middle);
            painter.line_segment([p(13.0, 1.0), p(13.0, 14.5)], line);
            // arm carrying the lamp house
            painter.line_segment([p(13.0, 4.0), p(10.5, 4.0)], line);
            painter.rect_stroke(egui::Rect::from_min_max(p(2.0, 1.5), p(10.5, 6.5)), 1.0 * k, line, egui::StrokeKind::Middle);
            // lens below the lamp house
            painter.rect_stroke(egui::Rect::from_min_max(p(4.75, 6.5), p(7.75, 9.0)), 0.5 * k, line, egui::StrokeKind::Middle);
        }
        DrawnIcon::TwoFields => {
            painter.rect_stroke(egui::Rect::from_min_max(p(1.0, 3.0), p(7.3, 13.0)), 1.0 * k, line, egui::StrokeKind::Middle);
            painter.rect_stroke(egui::Rect::from_min_max(p(8.7, 3.0), p(15.0, 13.0)), 1.0 * k, line, egui::StrokeKind::Middle);
        }
    }
}

/// Size of [`drawn_icon_button_at`] (as the toolbar's icon buttons).
pub const DRAWN_ICON_BUTTON: egui::Vec2 = egui::vec2(26.0, 24.0);

/// Like [`icon_button_hint`] (26 × 24, light fill when on, hint after a short hover delay),
/// with a vector symbol. Placed at a fixed `rect` (e.g. inside a row already allocated) without
/// moving the layout cursor; size [`DRAWN_ICON_BUTTON`].
pub fn drawn_icon_button_at(ui: &mut egui::Ui, rect: egui::Rect, salt: &str, icon: DrawnIcon, on: bool, tip: &str) -> bool {
    let resp = ui.interact(rect, ui.scope_id().with(("lr-drawn-icon", salt)), egui::Sense::click());
    let enabled = ui.is_enabled();
    let painter = ui.painter();
    let (fill, fg) = if on {
        (Color32::from_gray(0x38), Color32::WHITE)
    } else if resp.hovered() && enabled {
        (Color32::from_gray(0x2a), Color32::from_gray(0xe8))
    } else {
        (Color32::TRANSPARENT, if enabled { TEXT } else { Color32::from_gray(0x5c) })
    };
    painter.rect_filled(rect, 0.0, fill);
    if on || resp.has_focus() {
        painter.rect_stroke(rect, 0.0, egui::Stroke::new(if resp.has_focus() { 1.5 } else { 1.0 }, ACCENT), egui::StrokeKind::Inside);
    }
    paint_drawn_icon(painter, egui::Rect::from_center_size(rect.center(), egui::vec2(16.0, 16.0)), icon, fg);
    if !tip.is_empty() && enabled {
        hover_tip(ui, &resp, tip);
    }
    resp.clicked() && enabled
}

/// Entry of a context or pop-up menu: text on the left, shortcut right-aligned in its own column (dimmed).
/// Commands that open a dialog end with "…" in their text. Closes the menu when clicked. Returns: clicked.
pub fn menu_item(ui: &mut egui::Ui, text: &str, shortcut: &str, enabled: bool) -> bool {
    let mut b = egui::Button::new(text);
    if !shortcut.is_empty() {
        b = b.shortcut_text(egui::RichText::new(shortcut).color(TEXT_DIM));
    }
    let clicked = ui.add_enabled(enabled, b).clicked();
    if clicked {
        ui.close();
    }
    clicked
}

/// [`menu_item`] that, while disabled, tells why: `why` as a tooltip after the usual delay ([`hover_tip`]). Enabled
/// entries look and act as [`menu_item`].
pub fn menu_item_why(ui: &mut egui::Ui, text: &str, shortcut: &str, enabled: bool, why: &str) -> bool {
    let mut b = egui::Button::new(text);
    if !shortcut.is_empty() {
        b = b.shortcut_text(egui::RichText::new(shortcut).color(TEXT_DIM));
    }
    let resp = ui.add_enabled(enabled, b);
    if !enabled && !why.is_empty() {
        // a disabled widget reports no hover: a hover area of its own over the same place
        let probe = ui.interact(resp.rect, resp.id.with("warum"), egui::Sense::hover());
        hover_tip(ui, &probe, why);
    }
    let clicked = resp.clicked();
    if clicked {
        ui.close();
    }
    clicked
}

/// Input field (text or number) like the search field: dark surface [`FIELD_FILL`] with a 1 px frame
/// [`FIELD_STROKE`], lighter while it has the focus. Buttons stay frameless (surface only).
pub fn text_field(ui: &mut egui::Ui, widget: impl egui::Widget) -> egui::Response {
    let r = ui
        .scope(|ui| {
            // number fields (DragValue) are drawn like buttons: give them the field surface too
            let w = &mut ui.visuals_mut().widgets.inactive;
            w.bg_fill = FIELD_FILL;
            w.weak_bg_fill = FIELD_FILL;
            ui.add(widget)
        })
        .inner;
    field_outline(ui, &r);
    r
}

/// Text fields share enough vertical padding to match the surrounding buttons.
pub fn edit_field(ui: &mut egui::Ui, widget: egui::TextEdit<'_>) -> egui::Response {
    text_field(ui, widget.margin(egui::vec2(4.0, 5.0)))
}

/// The 1 px frame of an input field around `r` (focus lighter).
pub fn field_outline(ui: &egui::Ui, r: &egui::Response) {
    if ui.is_rect_visible(r.rect) {
        let colour = if r.has_focus() { Color32::from_gray(0x9a) } else { FIELD_STROKE };
        ui.painter().rect_stroke(r.rect, 0.0, egui::Stroke::new(1.0, colour), egui::StrokeKind::Inside);
    }
}

/// Search field in the style of the library's: frame [`BAR_H`] high, magnifier inside on the left,
/// × to clear on the right once there is text. Returns the response of the text input.
pub fn search_field(ui: &mut egui::Ui, text: &mut String, salt: &str, hint: &str, width: f32) -> egui::Response {
    let id = ui.scope_id().with(("lr-search", salt));
    let (frame, _) = ui.allocate_exact_size(egui::vec2(width, BAR_H), egui::Sense::hover());
    let focus = ui.memory(|m| m.has_focus(id));
    ui.painter().rect(frame, 0.0, FIELD_FILL, egui::Stroke::new(1.0, if focus { Color32::from_gray(0x9a) } else { FIELD_STROKE }), egui::StrokeKind::Inside);
    let loupe = egui::pos2(frame.left() + 14.0, frame.center().y - 1.0);
    let lf = egui::Stroke::new(1.4, if focus { TEXT } else { TEXT_DIM });
    ui.painter().circle_stroke(loupe, 4.5, lf);
    ui.painter().line_segment([loupe + egui::vec2(3.4, 3.4), loupe + egui::vec2(7.5, 7.5)], lf);
    let empty = text.is_empty();
    let input = egui::Rect::from_min_max(frame.min + egui::vec2(28.0, 2.0), frame.max - egui::vec2(if empty { 8.0 } else { 26.0 }, 2.0));
    let r = ui.put(
        input,
        egui::TextEdit::singleline(text)
            .id(id)
            .frame(egui::Frame::NONE)
            .text_color(TEXT)
            .align(egui::Align2::LEFT_CENTER)
            .hint_text(egui::RichText::new(hint).color(TEXT_DIM))
            .desired_width(input.width()),
    );
    if !empty {
        let cross = egui::Rect::from_center_size(egui::pos2(frame.right() - 14.0, frame.center().y), egui::vec2(18.0, 18.0));
        let resp = ui.interact(cross, id.with("clear"), egui::Sense::click());
        let kf = egui::Stroke::new(1.4, if resp.hovered() { Color32::WHITE } else { TEXT_DIM });
        let c = cross.center();
        ui.painter().line_segment([c + egui::vec2(-3.5, -3.5), c + egui::vec2(3.5, 3.5)], kf);
        ui.painter().line_segment([c + egui::vec2(-3.5, 3.5), c + egui::vec2(3.5, -3.5)], kf);
        if resp.clicked() {
            text.clear();
            ui.memory_mut(|m| m.request_focus(id));
        }
    }
    r
}

/// Shared show/hide motion for panels: 0 hidden … 1 shown, about 180 ms ease-out, respects `animation_time = 0`.
pub fn slide(ctx: &egui::Context, id: &str, shown: bool) -> f32 {
    let duration = if ctx.global_style().animation_time > 0.0 { 0.18 } else { 0.0 };
    let t = ctx.animate_bool_with_time(egui::Id::new(("lr-slide", id)), shown, duration);
    if t > 0.0 && t < 1.0 {
        ctx.request_repaint();
    }
    1.0 - (1.0 - t).powi(3)
}

/// Content of a sliding panel: laid out at its full size and clipped, so it glides in instead of squeezing.
/// `from_left`: the content enters from the left edge (left panels), otherwise from the right/bottom.
pub fn slid(ui: &mut egui::Ui, full: egui::Vec2, from_left: bool, add: impl FnOnce(&mut egui::Ui)) {
    let r = ui.max_rect();
    let min = if from_left { egui::pos2(r.right() - full.x, r.top()) } else { r.min };
    let inner = egui::Rect::from_min_size(min, egui::vec2(full.x, r.height()));
    ui.set_clip_rect(ui.clip_rect().intersect(r));
    ui.scope_builder(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Min)), |ui| add(ui));
}

/// Import (primary, gold, arrow in) or export (secondary surface, arrow out) with a small arrow left of the text;
/// the text is shortened when the button is narrow.
pub fn arrow_button(ui: &mut egui::Ui, text: &str, width: f32, primary: bool) -> egui::Response {
    arrow_button_gold(ui, text, width, primary, primary)
}

/// [`arrow_button`] with the arrow (`primary`: into the tray) and the gold fill chosen separately (one gold button
/// per window: a view with its own main button turns the import button plain).
pub fn arrow_button_gold(ui: &mut egui::Ui, text: &str, width: f32, primary: bool, gold: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 24.0), egui::Sense::click());
    let dark = Color32::from_rgb(0x16, 0x14, 0x10);
    let (fill, fg) = if gold {
        (if resp.is_pointer_button_down_on() { Color32::from_rgb(0xa8, 0x8e, 0x50) } else if resp.hovered() { Color32::from_rgb(0xdc, 0xbf, 0x76) } else { GOLD }, dark)
    } else {
        (if resp.hovered() { Color32::from_gray(0x36) } else { BUTTON_FILL }, TEXT)
    };
    let p = ui.painter();
    p.rect_filled(rect, 0.0, fill);
    let font = egui::TextStyle::Button.resolve(ui.style());
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_string(), font, fg);
    job.wrap = egui::text::TextWrapping { max_width: (width - 34.0).max(10.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    let g = p.layout_job(job);
    let total = 14.0 + 6.0 + g.size().x;
    let x0 = rect.center().x - total / 2.0;
    let c = egui::pos2(x0 + 7.0, rect.center().y);
    let s = Stroke::new(1.4, fg);
    // arrow into a tray (import) or out of it (export)
    p.line_segment([egui::pos2(c.x - 6.0, c.y + 3.0), egui::pos2(c.x - 6.0, c.y + 6.0)], s);
    p.line_segment([egui::pos2(c.x - 6.0, c.y + 6.0), egui::pos2(c.x + 6.0, c.y + 6.0)], s);
    p.line_segment([egui::pos2(c.x + 6.0, c.y + 6.0), egui::pos2(c.x + 6.0, c.y + 3.0)], s);
    let (tip, tail) = if primary { (c.y + 2.5, c.y - 6.0) } else { (c.y - 6.0, c.y + 2.5) };
    p.line_segment([egui::pos2(c.x, tail), egui::pos2(c.x, tip)], s);
    let d = if primary { -3.0 } else { 3.0 };
    p.line_segment([egui::pos2(c.x - 3.0, tip + d), egui::pos2(c.x, tip)], s);
    p.line_segment([egui::pos2(c.x + 3.0, tip + d), egui::pos2(c.x, tip)], s);
    p.galley(egui::pos2(x0 + 20.0, rect.center().y - g.size().y / 2.0), g, fg);
    resp
}
