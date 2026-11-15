//! Temporary showing of a hidden panel while the mouse rests on its edge strip.
//!
//! The decision is a small state machine without any egui types besides `Rect`, so it can be tested with simulated
//! pointer positions and times. It does not depend on the (animated) size of the panel: the "inside" area is the strip
//! plus the full target area of the panel, so the gliding phase cannot close the panel by accident.

use egui::{Pos2, Rect};

/// Dwell on the strip before the panel appears.
pub const DWELL: f64 = 0.35;
/// Grace period outside strip and panel before the panel goes away again.
pub const GRACE: f64 = 0.2;
/// Tolerance around the strip and the panel.
pub const MARGIN: f32 = 14.0;

#[derive(Clone, Copy, Default, Debug)]
pub struct Peek {
    open: bool,
    /// the pointer rests on the strip since …
    since: Option<f64>,
    /// the pointer is outside strip and panel since …
    outside_since: Option<f64>,
    /// opening is only allowed after the pointer has been away from the strip once (after a click or a close)
    blocked: bool,
}

pub struct Input {
    pub now: f64,
    pub on_strip: bool,
    pub in_zone: bool,
    /// the panel is shown permanently (no temporary showing needed)
    pub pinned: bool,
    pub pointer_down: bool,
}

impl Peek {
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// After a click on the strip: close and wait until the pointer has left the strip.
    pub fn reset(&mut self) {
        *self = Peek { blocked: true, ..Peek::default() };
    }

    /// Whether a timer runs (the caller must repaint soon).
    pub fn pending(&self) -> bool {
        self.since.is_some() || self.outside_since.is_some()
    }

    /// One frame. Returns whether the panel is temporarily shown.
    pub fn step(&mut self, i: Input) -> bool {
        if i.pinned {
            *self = Peek { blocked: i.on_strip, ..Peek::default() };
            return false;
        }
        if self.open {
            if i.on_strip || i.in_zone || i.pointer_down {
                self.outside_since = None;
            } else {
                let t0 = *self.outside_since.get_or_insert(i.now);
                if i.now - t0 >= GRACE {
                    // closed: no reopening in the same frame, and not until the pointer left the strip
                    *self = Peek { blocked: i.on_strip, ..Peek::default() };
                }
            }
        } else if self.blocked {
            if !i.on_strip {
                self.blocked = false;
            }
        } else if i.on_strip {
            // small movements on the strip keep the start time
            let t0 = *self.since.get_or_insert(i.now);
            if i.now - t0 > DWELL {
                self.open = true;
                self.since = None;
                self.outside_since = None;
            }
        } else {
            self.since = None;
        }
        self.open
    }
}

/// The area that counts as "inside": strip, full target area of the panel (not its animated size), the panel as last
/// seen, plus tolerance. `side`: 0 top, 1 bottom, 2 left, 3 right. `extent`: full size of the panel across the edge.
pub fn zone(strip: Rect, side: usize, extent: f32, last: Option<Rect>) -> Rect {
    let target = match side {
        0 => Rect::from_min_max(strip.min, Pos2::new(strip.max.x, strip.max.y + extent)),
        1 => Rect::from_min_max(Pos2::new(strip.min.x, strip.min.y - extent), strip.max),
        2 => Rect::from_min_max(strip.min, Pos2::new(strip.max.x + extent, strip.max.y)),
        _ => Rect::from_min_max(Pos2::new(strip.min.x - extent, strip.min.y), strip.max),
    };
    let r = strip.union(target);
    last.map_or(r, |l| r.union(l)).expand(MARGIN)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip() -> Rect {
        Rect::from_min_max(Pos2::new(0.0, 40.0), Pos2::new(12.0, 800.0))
    }
    fn inp(now: f64, p: Pos2, zone: Rect, pinned: bool) -> Input {
        Input { now, on_strip: strip().contains(p), in_zone: zone.contains(p), pinned, pointer_down: false }
    }

    #[test]
    fn dwell_then_open_and_one_pixel_moves_keep_the_time() {
        let z = zone(strip(), 2, 300.0, None);
        let mut p = Peek::default();
        let mut t = 0.0;
        let mut x = 6.0;
        while t < 0.34 {
            assert!(!p.step(inp(t, Pos2::new(x, 100.0), z, false)));
            x = if x > 6.5 { 6.0 } else { 7.0 }; // 1 px back and forth
            t += 0.016;
        }
        let mut opened = false;
        while t < 0.5 {
            opened |= p.step(inp(t, Pos2::new(x, 100.0), z, false));
            t += 0.016;
        }
        assert!(opened);
    }

    #[test]
    fn gliding_phase_with_no_or_small_panel_rect_never_closes() {
        // before the first frame of the panel there is no rect, later only a few px wide: zone stays the same
        for last in [None, Some(Rect::from_min_max(Pos2::new(12.0, 40.0), Pos2::new(13.0, 800.0)))] {
            let z = zone(strip(), 2, 300.0, last);
            let mut p = Peek::default();
            let mut t = 0.0;
            for _ in 0..30 {
                p.step(inp(t, Pos2::new(6.0, 100.0), z, false));
                t += 0.016;
            }
            assert!(p.is_open());
            for _ in 0..60 {
                assert!(p.step(inp(t, Pos2::new(7.0, 100.0), z, false)), "closed at {t}");
                t += 0.016;
            }
        }
    }

    #[test]
    fn closing_needs_the_grace_period_and_does_not_reopen_at_once() {
        let z = zone(strip(), 2, 300.0, None);
        let mut p = Peek::default();
        let mut t = 0.0;
        for _ in 0..30 {
            p.step(inp(t, Pos2::new(6.0, 100.0), z, false));
            t += 0.016;
        }
        assert!(p.is_open());
        // leave into the image: stays open during the grace period
        let away = Pos2::new(600.0, 100.0);
        assert!(p.step(inp(t, away, z, false)));
        assert!(p.step(inp(t + 0.1, away, z, false)));
        assert!(!p.step(inp(t + 0.25, away, z, false)));
        // a quick pass back over the panel within the grace period cancels the closing
        let mut q = Peek::default();
        let mut t = 0.0;
        for _ in 0..30 {
            q.step(inp(t, Pos2::new(6.0, 100.0), z, false));
            t += 0.016;
        }
        q.step(inp(t, away, z, false));
        assert!(q.step(inp(t + 0.1, Pos2::new(100.0, 100.0), z, false)));
        assert!(q.step(inp(t + 0.5, Pos2::new(100.0, 100.0), z, false)));
        // back on the strip right after closing: reopens only after the full dwell time, never at once
        let mut r = Peek::default();
        let mut t = 0.0;
        for _ in 0..30 {
            r.step(inp(t, Pos2::new(6.0, 100.0), z, false));
            t += 0.016;
        }
        r.step(inp(t, away, z, false));
        assert!(!r.step(inp(t + 0.25, away, z, false)));
        for k in 0..20 {
            assert!(!r.step(inp(t + 0.3 + k as f64 * 0.016, Pos2::new(6.0, 100.0), z, false)));
        }
    }

    #[test]
    fn click_on_the_strip_hides_without_reopening() {
        let z = zone(strip(), 2, 300.0, None);
        let mut p = Peek::default();
        let on = Pos2::new(6.0, 100.0);
        let mut t = 0.0;
        // panel pinned, pointer on the strip for a long time, click hides it
        for _ in 0..60 {
            p.step(inp(t, on, z, true));
            t += 0.016;
        }
        p.reset();
        for _ in 0..120 {
            assert!(!p.step(inp(t, on, z, false)));
            t += 0.016;
        }
        // after leaving once, dwelling works again
        p.step(inp(t, Pos2::new(500.0, 100.0), z, false));
        let mut opened = false;
        for _ in 0..40 {
            t += 0.016;
            opened |= p.step(inp(t, on, z, false));
        }
        assert!(opened);
    }

    #[test]
    fn exact_border_of_the_tolerance() {
        let z = zone(strip(), 2, 300.0, None);
        assert!(z.contains(Pos2::new(312.0 + MARGIN, 100.0)));
        assert!(!z.contains(Pos2::new(312.0 + MARGIN + 0.5, 100.0)));
    }

    #[test]
    fn all_sides_have_a_zone_covering_strip_and_panel() {
        let h = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1000.0, 12.0));
        assert!(zone(h, 0, 90.0, None).contains(Pos2::new(500.0, 100.0)));
        let b = Rect::from_min_max(Pos2::new(0.0, 900.0), Pos2::new(1000.0, 912.0));
        assert!(zone(b, 1, 150.0, None).contains(Pos2::new(500.0, 760.0)));
        let r = Rect::from_min_max(Pos2::new(1500.0, 0.0), Pos2::new(1512.0, 900.0));
        assert!(zone(r, 3, 380.0, None).contains(Pos2::new(1140.0, 400.0)));
    }
}
