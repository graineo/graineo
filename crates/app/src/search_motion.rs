//! Soft transitions of the library grid while searching. Only opacity over time is animated – no
//! layout per photo, nothing is computed again:
//! - new results open like a curtain from top to bottom: each visible row fades in, a little after the one above;
//! - while a newer search is pending, the shown results are dimmed softly (they stay in place, nothing jumps);
//! - with "Reduce motion" of the system only a short cross-fade of the whole grid, no stagger.

use std::time::{Duration, Instant};

/// Fade-in of one row.
const ROW_FADE: f32 = 0.14;
/// Delay from one visible row to the next (the whole curtain stays below ~250 ms).
const STAGGER: f32 = 0.025;
const STAGGER_ROWS: usize = 4;
/// Cross-fade with "Reduce motion".
const REDUCED_FADE: f32 = 0.12;
/// Dimming of outdated results: target opacity, duration, and the wait before it starts (a quick search never dims).
const DIM_TO: f32 = 0.45;
const DIM_FADE: f32 = 0.18;
const DIM_WAIT: Duration = Duration::from_millis(180);

#[derive(Default)]
pub struct Motion {
    reveal: Option<Instant>,
    reduced: bool,
    /// a reveal is due when the list changes because a search result arrived
    pub pending: bool,
}

impl Motion {
    /// A new result is shown: start the curtain. Reads the system setting once per reveal.
    pub fn start(&mut self, now: Instant) {
        self.reveal = Some(now);
        self.reduced = crate::zoom::reduced_motion();
    }

    /// Opacity of a visible row (`row`: 0 for the topmost row in view) at `now`; `outdated`: since when the shown
    /// result is outdated by a pending search.
    pub fn alpha(&self, row: usize, now: Instant, outdated: Option<Instant>) -> f32 {
        let mut a = 1.0;
        if let Some(t0) = self.reveal {
            let t = now.saturating_duration_since(t0).as_secs_f32();
            a = if self.reduced {
                (t / REDUCED_FADE).clamp(0.0, 1.0)
            } else {
                let delay = STAGGER * row.min(STAGGER_ROWS) as f32;
                ease(((t - delay) / ROW_FADE).clamp(0.0, 1.0))
            };
        }
        if let Some(since) = outdated {
            let wait = now.saturating_duration_since(since);
            if wait > DIM_WAIT {
                let fade = if self.reduced { REDUCED_FADE } else { DIM_FADE };
                let k = ((wait - DIM_WAIT).as_secs_f32() / fade).clamp(0.0, 1.0);
                a *= 1.0 - (1.0 - DIM_TO) * ease(k);
            }
        }
        a
    }

    /// Still moving at `now`? (then the caller asks for the next frame)
    pub fn moving(&mut self, now: Instant, outdated: Option<Instant>) -> bool {
        if let Some(t0) = self.reveal {
            let end = if self.reduced { REDUCED_FADE } else { STAGGER * STAGGER_ROWS as f32 + ROW_FADE };
            if now.saturating_duration_since(t0).as_secs_f32() >= end {
                self.reveal = None;
            }
        }
        let dimming = outdated.is_some_and(|s| now.saturating_duration_since(s) < DIM_WAIT + Duration::from_secs_f32(DIM_FADE.max(REDUCED_FADE)));
        self.reveal.is_some() || dimming
    }
}

/// "Searching …": a thin strip along the lower edge of the search field with a segment gliding to and fro.
pub fn busy_strip(painter: &eframe::egui::Painter, frame: eframe::egui::Rect, t: f64) {
    use eframe::egui;
    let track = egui::Rect::from_min_max(egui::pos2(frame.left() + 1.0, frame.bottom() - 3.0), egui::pos2(frame.right() - 1.0, frame.bottom() - 1.0));
    painter.rect_filled(track, 0.0, egui::Color32::from_gray(0x2c));
    let w = track.width() * 0.28;
    // to and fro, softly turning at the ends
    let phase = ((t / 1.1).fract() * std::f64::consts::TAU).sin() as f32 * 0.5 + 0.5;
    let x = track.left() + (track.width() - w) * phase;
    painter.rect_filled(egui::Rect::from_min_size(egui::pos2(x, track.top()), egui::vec2(w, track.height())), 0.0, egui::Color32::from_gray(0x9a));
}

/// Soft start and end.
fn ease(x: f32) -> f32 {
    x * x * (3.0 - 2.0 * x)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curtain_from_top_to_bottom_and_dimming() {
        let t0 = Instant::now();
        let mut m = Motion::default();
        assert_eq!(m.alpha(0, t0, None), 1.0, "nothing to do: fully visible");
        m.reveal = Some(t0);
        let at = |s: f32| t0 + Duration::from_secs_f32(s);
        // the top row is ahead of the lower ones
        assert!(m.alpha(0, at(0.05), None) > m.alpha(3, at(0.05), None));
        assert_eq!(m.alpha(0, at(0.0), None), 0.0);
        // all done within 250 ms, also far down
        assert_eq!(m.alpha(20, at(0.25), None), 1.0);
        assert!(m.moving(at(0.1), None));
        assert!(!m.moving(at(0.3), None));
        // outdated: no dimming at first, then down to DIM_TO
        assert_eq!(m.alpha(0, at(0.1), Some(t0)), 1.0);
        assert!((m.alpha(0, at(1.0), Some(t0)) - DIM_TO).abs() < 1e-4);
        // reduced motion: no stagger
        m.reveal = Some(t0);
        m.reduced = true;
        assert_eq!(m.alpha(0, at(0.06), None), m.alpha(5, at(0.06), None));
        assert_eq!(m.alpha(5, at(0.13), None), 1.0);
    }
}
