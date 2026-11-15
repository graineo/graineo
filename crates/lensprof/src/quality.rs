//! Quality of a profile, 0–100 per part; the profile counts as good as its weakest part.
//! Levels: ≥ 80 "GUT", ≥ 60 "BRAUCHBAR", otherwise "UNSICHER".

use crate::own::{Quality, QualityPart};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Good,
    Usable,
    Uncertain,
}

impl Level {
    pub fn of(score: u8) -> Self {
        match score {
            80.. => Level::Good,
            60.. => Level::Usable,
            _ => Level::Uncertain,
        }
    }

    /// German display text (the key for `tr`).
    pub fn text(self) -> &'static str {
        match self {
            Level::Good => "GUT",
            Level::Usable => "BRAUCHBAR",
            Level::Uncertain => "UNSICHER",
        }
    }
}

fn clamp(v: f64) -> u8 {
    v.clamp(0.0, 100.0).round() as u8
}

/// Distortion: residual of the straightened lines (pixels) and spread of the lines over the image (0–1).
pub fn distortion(rms_after_px: f64, spread: f64) -> u8 {
    clamp(100.0 - 40.0 * rms_after_px - 150.0 * (0.9 - spread).max(0.0))
}

/// Lateral CA: number of edge points that went into the estimate (20 000 and more: full score).
pub fn lateral_ca(points: usize) -> u8 {
    clamp(40.0 + 60.0 * (points as f64 / 20_000.0).min(1.0))
}

/// Vignetting from a flat field: residual of the radial fit and spread along the rings (both relative).
pub fn vignette(rest: f64, angle_spread: f64) -> u8 {
    clamp(100.0 - 2000.0 * rest - 500.0 * angle_spread)
}

/// Total of the measured parts: the weakest part.
pub fn overall(parts: &[(QualityPart, u8)]) -> Option<Quality> {
    let score = parts.iter().map(|p| p.1).min()?;
    Some(Quality { score, parts: parts.to_vec() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weakest_part_counts() {
        let q = overall(&[(QualityPart::Distortion, 92), (QualityPart::Cast, 61), (QualityPart::Vignette, 88)]).unwrap();
        assert_eq!(q.score, 61);
        assert_eq!(Level::of(q.score), Level::Usable);
        assert_eq!(Level::of(80), Level::Good);
        assert_eq!(Level::of(59).text(), "UNSICHER");
        assert!(overall(&[]).is_none());
        assert_eq!(distortion(0.1, 1.0), 96);
        assert_eq!(lateral_ca(40_000), 100);
    }
}
