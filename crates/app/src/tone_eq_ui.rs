//! Section "Tonwert-Equalizer" in Edit (category "Licht & Farbe"): the main slider "Ausgleich" (highlights are never
//! touched when brightening, deep shadows never when darkening), below it a graph of the nine zones −8 … 0 EV, a value
//! row for the selected zone and a narrow row with the mask settings (smoothing, edges, "Maske"). Calculation in
//! `develop::tone_eq`; acts for Sigma and all other cameras alike.
//!
//! The graph shows the correction that acts: the base shape of the main slider plus the zone curve, drawn exactly as
//! the calculation interpolates it ([`Curve`]); the base shape alone dashed, behind both the histogram of the zone
//! brightness of the photo (the same mask and exposure offset the equalizer uses, collected while the preview is
//! rendered, see `develop::tone_eq::collect`). Every interaction changes only the zone values (stored as before in
//! `zones`), never the main slider. The mouse wheel is never taken: it scrolls the panel. Nothing is selected until a
//! click or a drag selects a point (gold); dragging moves it relative to where it was grabbed (⌥ fine, catches at 0),
//! ↑/↓ step it (⇧ coarse, ⌥ fine), ←/→ select the neighbour, a double-click on the point sets it to 0 and a
//! double-click on the value rows opens a number field. Esc, a click outside the graph, another photo or a collapsed
//! section end the selection. Graph after the tone equalizer of darktable
//! (Aurélien Pierre); own implementation.
//!
//! The section sits in Edit and not in the tool rail: it has no interaction in the image (sliders belong to a
//! category, the rail is for tools). For the same reason a click into the photo does not pick a zone: clicks in the
//! photo already belong to zoom, the colour picker and the tools of the rail.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use develop::tone_eq::{balance_ev, Curve, ToneEq, HIST_BINS, HIST_MIN_EV, HIST_PER_EV, MAX_EV, ZONES, ZONE_EV};
use develop::Settings;
use eframe::egui::{self, Color32, Stroke};

use crate::lr;

/// Title of the section (also the key of its help card and of the copy group).
pub const TITLE: &str = "Tonwert-Equalizer";

/// Names of the zones (fixed, short: they are in EV, the same in every language).
const ZONE_LABELS: [&str; ZONES] = ["−8 EV", "−7 EV", "−6 EV", "−5 EV", "−4 EV", "−3 EV", "−2 EV", "−1 EV", "0 EV"];

/// Hint of the main slider (also selects the help card of the section, see `help.rs`).
pub const BALANCE_HINT: &str = "+ hellt Schatten auf, Lichter bleiben · − dunkelt Lichter ab, Schatten bleiben";
/// Hint of the mask sliders (selects the help card of the mask, see `help.rs`).
const SMOOTHING_HINT: &str = "Größe der Maskenflächen in % der langen Bildkante";
const EDGES_HINT: &str = "Wie eng die Maske Kanten folgt – höher: weniger Lichthöfe";

/// The section (the same for Sigma and other cameras).
pub fn section(ui: &mut egui::Ui, s: &mut Settings, index: usize) {
    debug_assert_eq!(ZONE_EV.len(), ZONES);
    let d = ToneEq::default();
    test_preset(s);
    let changed = s.tone_eq != d || s.tone_eq_mask;
    let open = crate::env::var_os("GRAINEO_TONE_EQ_OPEN").is_some();
    let reset = lr::section_reset(ui, TITLE, open, changed, |ui| {
        let mut a = s.tone_eq.amount as f64;
        if lr::slider_hint(ui, "Ausgleich", &mut a, -2.0..=2.0, 0.0, 0.05, BALANCE_HINT) {
            s.tone_eq.amount = a as f32;
        }
        ui.add_space(4.0);
        graph(ui, s, index);
        ui.add_space(4.0);
        mask_row(ui, s, &d);
    });
    if reset {
        s.tone_eq = d;
        s.tone_eq_mask = false;
    }
}

/// Narrow row below the graph: smoothing and edge sharpness of the mask (short sliders, name above the track, value
/// to the right of the name) and "Maske" (show the mask). Their explanation is in the help card of the section.
fn mask_row(ui: &mut egui::Ui, s: &mut Settings, d: &ToneEq) {
    let width = ui.available_width();
    let toggle_w = ui.fonts_mut(|f| f.layout_no_wrap(crate::tr("Maske").to_owned(), egui::FontId::proportional(lr::UI_SIZE), crate::lr::TEXT_STRONG()).size().x) + 18.0;
    let gap = 10.0;
    let slider_w = ((width - toggle_w - 2.0 * gap) / 2.0).max(40.0);
    let (row, _) = ui.allocate_exact_size(egui::vec2(width, MASK_ROW_HEIGHT), egui::Sense::hover());
    let a = egui::Rect::from_min_size(row.min, egui::vec2(slider_w, MASK_ROW_HEIGHT));
    let b = egui::Rect::from_min_size(egui::pos2(a.right() + gap, row.top()), egui::vec2(slider_w, MASK_ROW_HEIGHT));
    if mini_slider(ui, a, "Glättung", &mut s.tone_eq.smoothing, 1.0..=25.0, d.smoothing, 0.5, 1, SMOOTHING_HINT) {
        s.tone_eq.smoothing = (s.tone_eq.smoothing / 0.5).round() * 0.5;
    }
    mini_slider(ui, b, "Kanten", &mut s.tone_eq.edges, 0.0..=1.0, d.edges, 0.01, 2, EDGES_HINT);
    let t = egui::Rect::from_min_max(egui::pos2(row.right() - toggle_w, row.center().y - 11.0), egui::pos2(row.right(), row.center().y + 11.0));
    let mut on = s.tone_eq_mask;
    let r = ui.put(t, egui::Button::new(crate::tr("Maske")).selected(on));
    if r.clicked() {
        on = !on;
    }
    s.tone_eq_mask = on;
    lr::hover_tip(ui, &r, "Maske zeigen: Zonen als Graustufen, Schwarz = −8 EV, Weiß = 0 EV");
}

/// Short slider for the mask row: name above the track (cut with "…" if the column is too narrow), value right of
/// the name, track below; drag to change, double-click for the default. Number format as the other sliders.
#[allow(clippy::too_many_arguments)]
fn mini_slider(ui: &mut egui::Ui, rect: egui::Rect, label: &str, value: &mut f32, range: std::ops::RangeInclusive<f32>, default: f32, step: f32, decimals: usize, hint: &str) -> bool {
    let (lo, hi) = (*range.start(), *range.end());
    let resp = ui.interact(rect, egui::Id::unique(("tone-eq-mini", label)), egui::Sense::click_and_drag());
    let before = *value;
    let track_y = rect.bottom() - 7.0;
    let (x0, x1) = (rect.left() + 4.0, rect.right() - 4.0);
    if resp.double_clicked() {
        *value = default;
        crate::history::reset_gesture();
    } else if resp.dragged() || resp.clicked() {
        if let Some(q) = ui.input(|i| i.pointer.interact_pos()) {
            let t = ((q.x - x0) / (x1 - x0)).clamp(0.0, 1.0);
            *value = (((lo + t * (hi - lo)) / step).round() * step).clamp(lo, hi);
        }
    }
    let painter = ui.painter_at(rect);
    let font = egui::FontId::proportional(lr::UI_SIZE - 1.0);
    let val = number(*value, decimals);
    let val_g = painter.layout_no_wrap(val, font.clone(), lr::TEXT_DIM());
    let name_w = (rect.width() - val_g.size().x - 6.0).max(10.0);
    let name = lr::middle_ellipsis_font(&painter, &crate::tr(label).to_uppercase(), &font, name_w);
    let color = if resp.hovered() || resp.dragged() { crate::lr::gray(0xe6) } else { lr::CATEGORY_LABEL() };
    painter.text(egui::pos2(rect.left(), rect.top() + 7.0), egui::Align2::LEFT_CENTER, name, font, color);
    painter.galley(egui::pos2(rect.right() - val_g.size().x, rect.top() + 7.0 - val_g.size().y / 2.0), val_g, lr::TEXT_DIM());
    painter.hline(x0..=x1, track_y, Stroke::new(2.0, crate::lr::gray(0x44)));
    let x = x0 + (*value - lo) / (hi - lo) * (x1 - x0);
    painter.rect_filled(egui::Rect::from_center_size(egui::pos2(x, track_y), egui::vec2(3.0, 10.0)), 1.0, crate::lr::gray(0xd8));
    if !resp.dragged() {
        lr::hover_tip(ui, &resp, hint);
    }
    *value != before
}

/// Height of the graph (fixed: no layout jump with or without histogram).
const GRAPH_HEIGHT: f32 = 132.0;
/// Height of the EV scale below the graph.
const SCALE_HEIGHT: f32 = 14.0;
/// Shown range of zone brightness in EV: half a zone beyond the outer points.
const SHOW_MIN: f32 = -8.5;
const SHOW_MAX: f32 = 0.5;
/// Shown range of the correction in EV (main slider and zones together; beyond it the line is clipped).
const SHOW_RANGE: f32 = 3.0;
/// Steps of the arrow keys in EV: plain, with ⇧ and with ⌥.
const KEY_STEP: f32 = 0.05;
const BIG_STEP: f32 = 0.25;
const FINE_STEP: f32 = 0.01;
/// Dragging with ⌥: the point moves a fifth of the pointer movement.
const FINE_FACTOR: f32 = 0.2;
/// Dragging without ⌥ catches the value at 0 within this distance (EV).
const ZERO_CATCH: f32 = 0.04;
/// Height of the two value lines below the graph.
const VALUE_HEIGHT: f32 = 34.0;
/// Height of the mask row.
const MASK_ROW_HEIGHT: f32 = 30.0;

/// Zone histogram of the open photo and when the graph was last drawn (the render job collects the histogram only
/// while the graph is on screen).
struct GraphState {
    drawn: Option<Instant>,
    /// photo index and histogram (bins of `develop::tone_eq::hist_bin`)
    hist: Option<(usize, Vec<u32>)>,
    /// photo for which a render was already requested to collect the histogram (never twice in a row)
    requested: Option<usize>,
}

static STATE: Mutex<GraphState> = Mutex::new(GraphState { drawn: None, hist: None, requested: None });

fn state() -> std::sync::MutexGuard<'static, GraphState> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Is the graph on screen (drawn in the last two seconds)? Then the preview render collects the zone histogram.
pub fn graph_wanted() -> bool {
    state().drawn.is_some_and(|t| t.elapsed() < Duration::from_secs(2))
}

/// Result of the preview render: the zone histogram of photo `index` (`None`: not available for this view).
pub fn store_hist(index: usize, hist: Option<Vec<u32>>) {
    let mut st = state();
    match hist {
        Some(h) if h.len() == HIST_BINS => st.hist = Some((index, h)),
        _ => {
            if st.hist.as_ref().is_some_and(|(i, _)| *i != index) {
                st.hist = None;
            }
        }
    }
}

/// Should the open photo be rendered once more to collect the zone histogram (graph shown, none for this photo yet)?
/// Answers `true` only once per photo.
pub fn needs_render(index: usize) -> bool {
    let mut st = state();
    let shown = st.drawn.is_some_and(|t| t.elapsed() < Duration::from_secs(2));
    if !shown || st.hist.as_ref().is_some_and(|(i, _)| *i == index) || st.requested == Some(index) {
        return false;
    }
    st.requested = Some(index);
    true
}

/// Value as the sliders show it: decimal separator of the language ("5,0", "0,50" in German).
fn number(v: f32, places: usize) -> String {
    use crate::menu::i18n::DeComma;
    format!("{:.places$}", v.abs()).de_comma()
}

/// Signed value ("+1,10", "−0,50", "0,00" in German), with the typographic minus of the EV labels.
fn signed(v: f32) -> String {
    use crate::menu::i18n::DeComma;
    if v.abs() >= 0.005 { format!("{v:+.2}").replace('-', "−").de_comma() } else { number(0.0, 2) }
}

/// Histogram folded onto `columns` columns over the shown range (counts outside go to the outer columns).
fn columns(hist: &[u32], columns: usize) -> Vec<f32> {
    let mut out = vec![0f32; columns];
    for (i, &n) in hist.iter().enumerate() {
        let ev = HIST_MIN_EV + (i as f32 + 0.5) / HIST_PER_EV as f32;
        let t = ((ev - SHOW_MIN) / (SHOW_MAX - SHOW_MIN)).clamp(0.0, 0.9999);
        out[(t * columns as f32) as usize] += n as f32;
    }
    out
}

/// Index of the zone closest to brightness `ev`.
fn zone_at(ev: f32) -> usize {
    ((ev - ZONE_EV[0]).round().max(0.0) as usize).min(ZONES - 1)
}

/// When the graph last had the keyboard focus (milliseconds since [`clock`]; 0 = never).
static FOCUS_MS: AtomicU64 = AtomicU64::new(0);

fn clock() -> u64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64 + 1
}

/// Does the graph have the keyboard focus (drawn with focus in the last half second)? Then the arrow keys belong to
/// it: the global arrow shortcuts (next/previous photo, preset cursor) leave them alone.
pub fn keyboard_focus() -> bool {
    let f = FOCUS_MS.load(Ordering::Relaxed);
    f != 0 && clock().saturating_sub(f) < 500
}

/// When Esc last ended the selection of a point (milliseconds since [`clock`]; 0 = never).
static ESC_MS: AtomicU64 = AtomicU64::new(0);

/// Did the graph just take Esc to end its selection? Then the global Esc (back to the grid, leave fullscreen) waits.
pub fn took_escape() -> bool {
    let f = ESC_MS.load(Ordering::Relaxed);
    f != 0 && clock().saturating_sub(f) < 300
}

/// Parse a typed value ("+1,25", "−0.5", "2").
fn parse_value(t: &str) -> Option<f32> {
    t.trim().replace('−', "-").replace(',', ".").trim_start_matches('+').parse::<f32>().ok().filter(|v| v.is_finite())
}

/// The graph: the correction that acts (main slider plus zones) over the zone histogram, the base shape of the main
/// slider dashed. Every interaction changes only zone values; moving a point takes its neighbours along by half (⌘:
/// only the point; between the zones the line is the curve the calculation uses).
fn graph(ui: &mut egui::Ui, s: &mut Settings, index: usize) {
    let width = ui.available_width();
    let (outer, _) = ui.allocate_exact_size(egui::vec2(width, GRAPH_HEIGHT + SCALE_HEIGHT), egui::Sense::hover());
    let rect = egui::Rect::from_min_size(outer.min, egui::vec2(width, GRAPH_HEIGHT));
    // fixed ID (the section header gets more controls on a change, an automatic ID would abort the drag)
    let id = egui::Id::unique("tone-eq-graph");
    let resp = ui.interact(rect, id, egui::Sense::click_and_drag());
    let painter = ui.painter_at(outer);
    let x_of = |ev: f32| rect.left() + (ev - SHOW_MIN) / (SHOW_MAX - SHOW_MIN) * rect.width();
    let ev_of = |x: f32| SHOW_MIN + (x - rect.left()) / rect.width() * (SHOW_MAX - SHOW_MIN);
    let y_of = |v: f32| rect.center().y - v / SHOW_RANGE * (rect.height() / 2.0 - 6.0);
    let v_of = |y: f32| (rect.center().y - y) / (rect.height() / 2.0 - 6.0) * SHOW_RANGE;
    // base shape of the main slider "Ausgleich" at zone brightness `ev` (a surface of that brightness: the pixel's own
    // brightness equals its zone); the zones add to it
    let amount = s.tone_eq.amount;
    let base = move |ev: f32| if amount == 0.0 { 0.0 } else { balance_ev(amount, ev, ev) };
    let sel_id = id.with("selected");
    let editing_id = egui::Id::unique("tone-eq-value-edit");
    let editing = ui.data(|d| d.get_temp::<String>(editing_id)).is_some();
    // the selected point lives only as long as the graph keeps the keyboard focus (or its number field is open), for
    // this photo, and while the graph stays on screen (a collapsed section or another view ends it)
    let prev: Option<Selection> = ui.data(|d| d.get_temp(sel_id));
    let seen = state().drawn.is_some_and(|t| t.elapsed() < Duration::from_millis(500));
    state().drawn = Some(Instant::now());
    let mut sel: Option<usize> = prev.and_then(|p| p.keep(index, seen));
    if let Some(z) = test_select() {
        // test aid: what a click on the point does (select it, focus the graph)
        sel = Some(z);
        ui.memory_mut(|m| m.request_focus(id));
    }
    // background, histogram, grid
    painter.rect_filled(rect, 0.0, lr::PANEL_DARK());
    let hist = state().hist.as_ref().filter(|(i, _)| *i == index).map(|(_, h)| h.clone());
    if let Some(h) = hist {
        let n = (rect.width() / 2.0).round().max(8.0) as usize;
        let cols = columns(&h, n);
        let max = cols.iter().copied().fold(1.0f32, f32::max);
        for (i, &v) in cols.iter().enumerate() {
            let hgt = (v / max).sqrt() * rect.height() * 0.9;
            if hgt < 0.5 {
                continue;
            }
            let x0 = rect.left() + rect.width() * i as f32 / n as f32;
            let x1 = rect.left() + rect.width() * (i + 1) as f32 / n as f32;
            painter.rect_filled(egui::Rect::from_min_max(egui::pos2(x0, rect.bottom() - hgt), egui::pos2(x1 + 0.3, rect.bottom())), 0.0, crate::lr::overlay(20));
        }
    }
    let grid = Stroke::new(1.0, crate::lr::gray(0x2c));
    for ev in ZONE_EV {
        painter.vline(x_of(ev), rect.y_range(), grid);
    }
    for v in [-2.0, -1.0, 1.0, 2.0] {
        painter.hline(rect.x_range(), y_of(v), grid);
    }
    painter.hline(rect.x_range(), y_of(0.0), Stroke::new(1.0, crate::lr::gray(0x48)));
    // keyboard focus (arrow keys act only then): one step brighter than the normal frame
    let frame = if resp.has_focus() { crate::lr::gray(0x5a) } else { crate::lr::gray(0x4a) };
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0, frame), egui::StrokeKind::Inside);
    let small = egui::FontId::proportional(9.5);
    painter.text(egui::pos2(rect.left() + 3.0, y_of(2.0)), egui::Align2::LEFT_BOTTOM, "+2", small.clone(), lr::TEXT_DIM());
    painter.text(egui::pos2(rect.left() + 3.0, y_of(-2.0)), egui::Align2::LEFT_TOP, "−2", small.clone(), lr::TEXT_DIM());
    // EV scale below: −8 … 0 (highlights on the right)
    for ev in ZONE_EV {
        let t = if ev == 0.0 { "0 EV".to_string() } else { format!("−{}", -ev as i32) };
        let (x, align) = if ev == 0.0 { (outer.right(), egui::Align2::RIGHT_CENTER) } else { (x_of(ev), egui::Align2::CENTER_CENTER) };
        painter.text(egui::pos2(x, rect.bottom() + SCALE_HEIGHT / 2.0 + 1.0), align, t, small.clone(), lr::TEXT_DIM());
    }

    // interaction: the mouse wheel is never taken (it scrolls the panel); only a click or a drag selects a point and
    // only a selected point changes (drag, arrow keys, double-click, number field)
    let pointer = ui.input(|i| i.pointer.interact_pos().or(i.pointer.hover_pos()));
    let near = |q: egui::Pos2, zones: &[f32; ZONES]| -> Option<usize> {
        let i = zone_at(ev_of(q.x));
        (egui::pos2(x_of(ZONE_EV[i]), y_of(base(ZONE_EV[i]) + zones[i])).distance(q) <= 10.0).then_some(i)
    };
    let drag_id = id.with("drag");
    if resp.clicked() || resp.drag_started() || resp.double_clicked() {
        resp.request_focus();
        // the nearest point, or the zone under the pointer (a column is easy to hit)
        if let Some(q) = pointer {
            let z = near(q, &s.tone_eq.zones).unwrap_or_else(|| zone_at(ev_of(q.x)));
            sel = Some(z);
            if resp.drag_started() {
                // relative drag: the point never jumps to the pointer, it follows the movement from here (all zones
                // kept, so the neighbours that follow are moved from their own start values)
                ui.data_mut(|d| d.insert_temp(drag_id, (s.tone_eq.zones, q.y)));
            }
        }
    }
    if let Some(z) = sel {
        if resp.double_clicked() {
            if pointer.and_then(|q| near(q, &s.tone_eq.zones)) == Some(z) {
                s.tone_eq.zones[z] = 0.0;
                crate::history::reset_gesture();
            }
        } else if resp.dragged() {
            let start: Option<([f32; ZONES], f32)> = ui.data(|d| d.get_temp(drag_id));
            if let (Some((z0, y0)), Some(q)) = (start, ui.input(|inp| inp.pointer.interact_pos())) {
                let (fine, solo) = ui.input(|i| (i.modifiers.alt, i.modifiers.command));
                let v = drag_value(z0[z], v_of(q.y) - v_of(y0), fine);
                s.tone_eq.zones = with_neighbours(&z0, z, v - z0[z], !solo);
            }
        }
    }
    if resp.drag_stopped() {
        ui.data_mut(|d| d.remove::<([f32; ZONES], f32)>(drag_id));
    }
    // keyboard (only while the graph has the focus): ↑/↓ step the selected zone, ←/→ select the neighbour, Esc ends
    if resp.has_focus() {
        FOCUS_MS.store(clock(), Ordering::Relaxed);
        ui.memory_mut(|m| m.set_focus_lock_filter(id, egui::EventFilter { horizontal_arrows: true, vertical_arrows: true, escape: true, ..Default::default() }));
        let mut step = 0.0;
        let mut solo = false;
        let mut moved = 0i32;
        let mut esc = false;
        ui.input_mut(|i| {
            for (m, k) in [(egui::Modifiers::NONE, KEY_STEP), (egui::Modifiers::SHIFT, BIG_STEP), (egui::Modifiers::ALT, FINE_STEP)] {
                while i.consume_key(m, egui::Key::ArrowUp) {
                    step += k;
                }
                while i.consume_key(m, egui::Key::ArrowDown) {
                    step -= k;
                }
            }
            // ⌘: only this point, the neighbours stay
            while i.consume_key(egui::Modifiers::COMMAND, egui::Key::ArrowUp) {
                step += KEY_STEP;
                solo = true;
            }
            while i.consume_key(egui::Modifiers::COMMAND, egui::Key::ArrowDown) {
                step -= KEY_STEP;
                solo = true;
            }
            while i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight) {
                moved += 1;
            }
            while i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft) {
                moved -= 1;
            }
            esc = i.consume_key(egui::Modifiers::NONE, egui::Key::Escape);
        });
        if esc && !editing {
            sel = None;
            ESC_MS.store(clock(), Ordering::Relaxed);
            ui.memory_mut(|m| m.surrender_focus(id));
        } else if moved != 0 || step != 0.0 || sel.is_some() {
            // focus without a selection (reached with Tab): the arrow keys start at the middle zone
            let z = (sel.unwrap_or(ZONES / 2) as i32 + moved).clamp(0, ZONES as i32 - 1) as usize;
            sel = Some(z);
            if step != 0.0 {
                let v = snap(s.tone_eq.zones[z] + step);
                s.tone_eq.zones = with_neighbours(&s.tone_eq.zones, z, v - s.tone_eq.zones[z], !solo);
            }
        }
    } else if !editing && !resp.dragged() {
        // focus elsewhere (click outside the graph, another control, Tab): nothing stays selected
        sel = None;
    }
    ui.data_mut(|d| match sel {
        Some(zone) => {
            d.insert_temp(sel_id, Selection { index, zone });
        }
        None => {
            d.remove::<Selection>(sel_id);
        }
    });

    // the base shape of the main slider alone, dashed (what comes from "Ausgleich")
    let steps = (rect.width() / 2.0).max(16.0) as usize;
    let at = |k: usize| SHOW_MIN + (SHOW_MAX - SHOW_MIN) * k as f32 / steps as f32;
    if amount != 0.0 {
        let shape: Vec<egui::Pos2> = (0..=steps).map(|k| egui::pos2(x_of(at(k)), y_of(base(at(k).clamp(ZONE_EV[0], 0.0))))).collect();
        painter.extend(egui::Shape::dashed_line(&shape, Stroke::new(1.0, crate::lr::gray(0x80)), 4.0, 3.0));
    }
    // the correction that acts: main slider plus zones, exactly as calculated (flat beyond −8 and 0 EV)
    let curve = Curve::new(&s.tone_eq.zones);
    let line: Vec<egui::Pos2> = (0..=steps)
        .map(|k| {
            let ev = at(k).clamp(ZONE_EV[0], 0.0);
            egui::pos2(x_of(at(k)), y_of(base(ev) + curve.ev(ev)))
        })
        .collect();
    painter.add(egui::Shape::line(line, Stroke::new(1.5, lr::GOLD())));
    // the points (base shape plus zone value); the selected one gold, a hovered one lighter (display only)
    let hovered = if resp.hovered() && !resp.dragged() { pointer.and_then(|q| near(q, &s.tone_eq.zones)) } else { None };
    let hovered_col = if resp.hovered() { pointer.map(|q| zone_at(ev_of(q.x))) } else { None };
    for i in 0..ZONES {
        let c = egui::pos2(x_of(ZONE_EV[i]), y_of(base(ZONE_EV[i]) + s.tone_eq.zones[i]));
        if sel == Some(i) {
            painter.circle(c, 4.5, lr::GOLD(), Stroke::new(1.0, lr::PANEL_DARK()));
        } else {
            let fill = if hovered == Some(i) { crate::lr::gray(0xf0) } else { crate::lr::gray(0xb8) };
            painter.circle(c, 3.5, fill, Stroke::new(1.0, lr::PANEL_DARK()));
        }
    }
    // no tooltip: it would cover the value rows; the controls are in the help card of the section.
    // The value rows show the selected zone; without a selection the zone under the pointer (dimmed, read only).
    let shown = sel.map(|z| (z, true)).or_else(|| hovered_col.map(|z| (z, false)));
    if value_row(ui, s, shown.map(|(z, active)| (z, base(ZONE_EV[z]), active))) {
        // back to the graph after Enter/Esc in the number field: the point stays selected
        ui.memory_mut(|m| m.request_focus(id));
    }
}

/// Rows below the graph (two fixed lines, no layout jump, short enough for every language): the zone and the
/// correction that acts there, below it the parts (main slider and point). `shown`: zone, base shape there and
/// whether it is the selected point (else the zone under the pointer, dimmed). Double-click on the selected point's
/// rows opens a number field for the zone value (Enter takes it, Esc cancels). Returns whether a field just closed.
fn value_row(ui: &mut egui::Ui, s: &mut Settings, shown: Option<(usize, f32, bool)>) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), VALUE_HEIGHT), egui::Sense::click());
    let edit_id = egui::Id::unique("tone-eq-value-edit");
    let mut editing: Option<String> = ui.data(|d| d.get_temp(edit_id));
    let mut closed = false;
    let font = egui::FontId::proportional(lr::UI_SIZE - 0.5);
    let line1 = egui::pos2(rect.left(), rect.top() + VALUE_HEIGHT * 0.27);
    let line2 = egui::pos2(rect.left(), rect.top() + VALUE_HEIGHT * 0.75);
    let painter = ui.painter_at(rect);
    match shown {
        None => {
            editing = None;
            painter.text(line1, egui::Align2::LEFT_CENTER, "–", font, lr::TEXT_DIM());
        }
        Some((sel, base, active)) => {
            let v = s.tone_eq.zones[sel];
            if !active {
                editing = None;
            } else if resp.double_clicked() && editing.is_none() {
                editing = Some(signed(v));
            }
            let head = crate::trf!("Zone {z}", z = ZONE_LABELS[sel]);
            match editing.as_mut() {
                Some(text) => {
                    painter.text(line1, egui::Align2::LEFT_CENTER, &head, font.clone(), lr::TEXT_DIM());
                    painter.text(line2, egui::Align2::LEFT_CENTER, crate::tr("Punkt"), font, lr::TEXT_DIM());
                    let field = egui::Rect::from_min_max(egui::pos2(rect.right() - 72.0, rect.center().y - 10.0), egui::pos2(rect.right(), rect.center().y + 10.0));
                    let te = ui.put(field, egui::TextEdit::singleline(text).desired_width(68.0).align(egui::Align2::RIGHT_CENTER).id(edit_id.with("field")));
                    if !te.has_focus() && !te.lost_focus() {
                        te.request_focus();
                    }
                    let (enter, esc) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
                    if te.lost_focus() || enter || esc {
                        if !esc {
                            if let Some(x) = parse_value(text) {
                                s.tone_eq.zones[sel] = snap(x);
                            }
                        }
                        editing = None;
                        closed = true;
                    }
                }
                None => {
                    let strong = if active { crate::lr::gray(0xd0) } else { lr::TEXT_DIM() };
                    painter.text(line1, egui::Align2::LEFT_CENTER, &head, font.clone(), strong);
                    painter.text(egui::pos2(rect.right(), line1.y), egui::Align2::RIGHT_CENTER, signed(base + v), font.clone(), strong);
                    let parts = crate::trf!("Ausgleich {b} · Punkt {v}", b = signed(base), v = signed(v));
                    painter.text(line2, egui::Align2::LEFT_CENTER, parts, font, lr::TEXT_DIM());
                }
            }
        }
    }
    ui.data_mut(|d| match editing {
        Some(t) => {
            d.insert_temp(edit_id, t);
        }
        None => {
            d.remove::<String>(edit_id);
        }
    });
    closed
}

/// The selected point: photo and zone (temporary UI state, never stored with the photo).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Selection {
    index: usize,
    zone: usize,
}

impl Selection {
    /// Does the selection survive into this frame? Only for the same photo and while the graph stayed on screen.
    fn keep(self, index: usize, seen: bool) -> Option<usize> {
        (self.index == index && seen).then_some(self.zone.min(ZONES - 1))
    }
}

/// Zone value rounded to 0.01 EV and limited.
fn snap(v: f32) -> f32 {
    ((v / 0.01).round() * 0.01).clamp(-MAX_EV, MAX_EV)
}

/// Share of a point's movement that its two neighbours take along (soft editing: a single zone rarely wants to move
/// alone; ⌘ moves only the point). It is the value at one zone distance of a smooth raised-cosine falloff that reaches
/// zero at two zones, so overlapping edits blend like overlapping range weights instead of forming steps (credit in
/// the changelog); it only shapes the editing gesture, the stored zones and the calculation are unchanged.
const NEIGHBOUR_SHARE: f32 = 0.5;

/// Zones after moving zone `z` by `delta` from `start`: with `follow` the direct neighbours move by
/// [`NEIGHBOUR_SHARE`] of it (limited and rounded like every zone value), the other zones stay.
fn with_neighbours(start: &[f32; ZONES], z: usize, delta: f32, follow: bool) -> [f32; ZONES] {
    let mut out = *start;
    out[z] = snap(start[z] + delta);
    if follow {
        for n in [z.wrapping_sub(1), z + 1] {
            if n < ZONES {
                out[n] = snap(start[n] + delta * NEIGHBOUR_SHARE);
            }
        }
    }
    out
}

/// Zone value while dragging: start value plus the vertical movement (in EV of the graph), with ⌥ a fifth of it.
/// Without ⌥ the value catches at 0 (within [`ZERO_CATCH`]), so the neutral point is easy to hit.
fn drag_value(start: f32, moved: f32, fine: bool) -> f32 {
    let v = start + if fine { moved * FINE_FACTOR } else { moved };
    if !fine && v.abs() < ZERO_CATCH {
        0.0
    } else {
        snap(v)
    }
}

/// Test aid (own data folder only): `GRAINEO_TONE_EQ_TEST="amount=1.5;zones=…;mask=1"` sets the main slider, the
/// zones (from −8 EV to 0 EV) and "show mask" once, for screenshots.
fn test_preset(s: &mut Settings) {
    static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if crate::env::var_os("GRAINEO_DATA_DIR").is_none() || DONE.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let Ok(v) = crate::env::var("GRAINEO_TONE_EQ_TEST") else { return };
    DONE.store(true, std::sync::atomic::Ordering::Relaxed);
    for part in v.split(';') {
        match part.split_once('=') {
            Some(("zones", z)) => {
                for (i, x) in z.split(',').filter_map(|x| x.trim().parse::<f32>().ok()).take(ZONES).enumerate() {
                    s.tone_eq.zones[i] = x;
                }
            }
            Some(("mask", m)) => s.tone_eq_mask = m == "1",
            Some(("amount", a)) => s.tone_eq.amount = a.trim().parse().unwrap_or(0.0),
            _ => {}
        }
    }
}

/// Test aid (own data folder only): `select=<zone 0–8>@<seconds>` in `GRAINEO_TONE_EQ_TEST` selects that point once,
/// the given seconds after the first call (as a click would), for screenshots of the selected state.
fn test_select() -> Option<usize> {
    static FIRST: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if crate::env::var_os("GRAINEO_DATA_DIR").is_none() || DONE.load(Ordering::Relaxed) {
        return None;
    }
    let v = crate::env::var("GRAINEO_TONE_EQ_TEST").ok()?;
    let (z, t) = v.split(';').find_map(|p| p.strip_prefix("select="))?.split_once('@')?;
    let (z, t): (usize, f32) = (z.trim().parse().ok()?, t.trim().parse().ok()?);
    if FIRST.get_or_init(Instant::now).elapsed().as_secs_f32() < t {
        return None;
    }
    DONE.store(true, Ordering::Relaxed);
    Some(z.min(ZONES - 1))
}

/// The mask view is for the screen only: jobs that write files or thumbnails never show it.
pub fn without_mask(s: &mut Settings) {
    s.tone_eq_mask = false;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_match_zones() {
        for (i, ev) in ZONE_EV.iter().enumerate() {
            let n = ev.abs() as i32;
            assert!(ZONE_LABELS[i].starts_with(if n == 0 { "0" } else { "−" }));
            assert!(ZONE_LABELS[i].contains(&format!("{n} EV")));
            assert_eq!(zone_at(*ev), i);
        }
    }

    #[test]
    fn selection_ends_with_photo_or_hidden_graph() {
        let s = Selection { index: 3, zone: 5 };
        assert_eq!(s.keep(3, true), Some(5));
        assert_eq!(s.keep(4, true), None);
        assert_eq!(s.keep(3, false), None);
    }

    #[test]
    fn drag_is_relative_fine_and_catches_zero() {
        assert_eq!(drag_value(0.5, 0.0, false), 0.5);
        assert!((drag_value(0.5, 0.25, false) - 0.75).abs() < 1e-6);
        assert!((drag_value(0.5, 0.25, true) - 0.55).abs() < 1e-6);
        assert_eq!(drag_value(0.2, -0.17, false), 0.0);
        assert!((drag_value(0.2, -0.17, true) - 0.17).abs() < 1e-6);
        assert_eq!(drag_value(0.0, 10.0, false), MAX_EV);
    }

    #[test]
    fn neighbours_follow_by_half_unless_solo() {
        let z0 = [0.0; ZONES];
        let a = with_neighbours(&z0, 4, 1.0, true);
        assert_eq!((a[3], a[4], a[5]), (0.5, 1.0, 0.5));
        assert_eq!((a[2], a[6]), (0.0, 0.0));
        let b = with_neighbours(&z0, 4, 1.0, false);
        assert_eq!((b[3], b[4], b[5]), (0.0, 1.0, 0.0));
        // edge zones have one neighbour; a neighbour keeps its own start value
        let mut z1 = z0;
        z1[1] = 0.3;
        let c = with_neighbours(&z1, 0, -0.4, true);
        assert!((c[0] + 0.4).abs() < 1e-6 && (c[1] - 0.1).abs() < 1e-6);
        let d = with_neighbours(&z0, ZONES - 1, 0.2, true);
        assert!((d[ZONES - 2] - 0.1).abs() < 1e-6);
    }

    #[test]
    fn signed_uses_typographic_minus() {
        assert!(!signed(-0.15).contains('-'));
        assert!(signed(-0.15).starts_with('−'));
        assert_eq!(parse_value(&signed(-0.15)), Some(-0.15));
    }

    #[test]
    fn typed_values() {
        assert_eq!(parse_value("+1,25"), Some(1.25));
        assert_eq!(parse_value("−0.5"), Some(-0.5));
        assert_eq!(parse_value(" 2 "), Some(2.0));
        assert_eq!(parse_value("x"), None);
    }

    #[test]
    fn histogram_columns_keep_counts() {
        let h: Vec<u32> = (0..HIST_BINS as u32).collect();
        let c = columns(&h, 37);
        assert_eq!(c.iter().sum::<f32>() as u64, h.iter().map(|&v| v as u64).sum::<u64>());
    }
}
