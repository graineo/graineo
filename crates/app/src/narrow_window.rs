//! Narrow window: when the window becomes narrower than [`THRESHOLD`], one side panel collapses
//! automatically, once. If the user opens it again it stays open; the automatic collapse only
//! comes back after the window was made wider than the threshold and then narrower again.

/// Width in points below which a side panel collapses by itself.
pub const THRESHOLD: f32 = 1100.0;

/// Remembers whether the window was already below the threshold (the collapse happened).
#[derive(Default)]
pub struct Auto {
    narrow: bool,
}

/// Which panel to collapse.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Side {
    Left,
    Right,
}

impl Auto {
    /// State "collapse already used up": the automatic collapse stays out of the way
    /// (the layout tests of the app run in narrow test windows and expect fixed panels).
    pub fn used_up() -> Self {
        Auto { narrow: true }
    }

    /// Called every frame with the window width and the state of the two panels (`true` = open).
    /// Returns the panel to collapse, only at the moment the threshold is crossed downwards:
    /// the left one first, the right one only if the left is already closed.
    pub fn step(&mut self, width: f32, left_open: bool, right_open: bool) -> Option<Side> {
        if width >= THRESHOLD {
            self.narrow = false;
            return None;
        }
        if self.narrow {
            return None;
        }
        self.narrow = true;
        if left_open {
            Some(Side::Left)
        } else if right_open {
            Some(Side::Right)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapses_once_on_crossing() {
        let mut a = Auto::default();
        assert_eq!(a.step(1500.0, true, true), None);
        assert_eq!(a.step(1050.0, true, true), Some(Side::Left));
        // user opens it again, window stays narrow: nothing happens
        assert_eq!(a.step(1040.0, true, true), None);
        assert_eq!(a.step(900.0, true, true), None);
    }

    #[test]
    fn comes_back_only_after_widening() {
        let mut a = Auto::default();
        assert_eq!(a.step(1000.0, true, true), Some(Side::Left));
        assert_eq!(a.step(1100.0, true, true), None); // exactly at the threshold counts as wide
        assert_eq!(a.step(1099.0, true, true), Some(Side::Left));
    }

    #[test]
    fn right_panel_if_left_is_closed() {
        let mut a = Auto::default();
        assert_eq!(a.step(1000.0, false, true), Some(Side::Right));
        let mut b = Auto::default();
        assert_eq!(b.step(1000.0, false, false), None);
        assert_eq!(b.step(1000.0, true, true), None, "crossing already used up");
    }
}
