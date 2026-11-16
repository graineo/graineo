//! Soft crossfade from the fast preview to the exact version of the same photo.
//!
//! The old picture stays underneath, the new one is drawn over it with alpha 0 -> 1. The state lives in `Current`
//! (so it ends with the photo), the old texture is released when the fade is over. No difference check, no
//! repaint outside the fade.

/// Length of the fade in seconds.
pub const DURATION: f64 = 0.2;
/// A fade armed this long ago (exact base arrived, render never followed) is no longer started.
pub const ARM_WINDOW: f64 = 5.0;

/// Ease-out curve (smoothstep), 0 -> 0 and 1 -> 1, monotonic.
pub fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Duration for this run: `GRAINEO_FADE_TEST=<ms>` lengthens it for screenshots, reduced motion shortens it to 0.
pub fn duration() -> f64 {
    if crate::zoom::reduced_motion() {
        return 0.0;
    }
    match crate::env::var("GRAINEO_FADE_TEST").ok().and_then(|v| v.trim().parse::<f64>().ok()) {
        Some(ms) if ms > 0.0 => ms / 1000.0,
        _ => DURATION,
    }
}

/// Pure time state of a running fade.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Clock {
    start: f64,
    duration: f64,
}

impl Clock {
    /// `None` when there is nothing to fade (duration 0).
    pub fn start(now: f64, duration: f64) -> Option<Clock> {
        (duration > 0.0).then_some(Clock { start: now, duration })
    }

    /// Alpha of the new picture, or `None` when the fade is over.
    pub fn alpha(&self, now: f64) -> Option<f32> {
        let t = ((now - self.start) / self.duration) as f32;
        (t < 1.0).then(|| ease(t.max(0.0)))
    }
}

/// Fade of one photo: old texture below, clock for the new one on top.
pub struct Fade {
    pub old: egui::TextureHandle,
    clock: Clock,
}

impl Fade {
    pub fn new(old: egui::TextureHandle, now: f64) -> Option<Fade> {
        Clock::start(now, duration()).map(|clock| Fade { old, clock })
    }

    pub fn alpha(&self, now: f64) -> Option<f32> {
        self.clock.alpha(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curve_ends_and_is_monotonic() {
        assert_eq!(ease(0.0), 0.0);
        assert_eq!(ease(1.0), 1.0);
        assert_eq!(ease(-1.0), 0.0);
        assert_eq!(ease(2.0), 1.0);
        let mut last = 0.0;
        for i in 1..=100 {
            let v = ease(i as f32 / 100.0);
            assert!(v >= last);
            last = v;
        }
    }

    #[test]
    fn duration_is_200_ms() {
        assert_eq!(DURATION, 0.2);
    }

    #[test]
    fn clock_start_middle_end() {
        let c = Clock::start(10.0, DURATION).unwrap();
        assert_eq!(c.alpha(10.0), Some(0.0));
        let mid = c.alpha(10.1).unwrap();
        assert!((mid - 0.5).abs() < 1e-3);
        assert_eq!(c.alpha(10.2), None);
        assert_eq!(c.alpha(11.0), None);
    }

    #[test]
    fn no_clock_without_duration() {
        assert!(Clock::start(0.0, 0.0).is_none());
    }
}
