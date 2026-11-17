//! Does a picture region show sky? A zero-shot check with SigLIP 2 Base (the model of the search and the keyword
//! suggestions, `content_keywords::MODEL`): the region (rest of the picture neutral grey) is compared with a few
//! short descriptions of sky and of what is often mistaken for it (water, snow, a wall, a backdrop …). The result is
//! the share of the sky descriptions in the softmax over all of them (0..1). Only used to confirm an automatic sky
//! selection (`segment::sky`); never downloads anything, without the installed model there is no check.
use std::path::Path;

use crate::siglip::{logit_params, ImageTower, TextTower};

/// Descriptions of sky (first [`SKY`] entries) and of other regions, each as "a photo of {}.".
pub const TERMS: &[&str] = &[
    "the sky",
    "a blue sky",
    "a cloudy sky",
    "a sunset sky",
    "a hazy sky",
    "a mountain",
    "snow",
    "trees",
    "a forest",
    "water",
    "the sea",
    "a lake",
    "a building",
    "the ground",
    "sand",
    "grass",
    "a wall",
    "a person",
    "a flower",
    "a plant",
    "shoes",
    "fabric",
    "a plain studio backdrop",
];
/// Number of sky descriptions at the start of [`TERMS`].
pub const SKY: usize = 5;

/// Loaded model and the text vectors of [`TERMS`].
pub struct Scorer {
    image: ImageTower,
    terms: Vec<Vec<f32>>,
    scale: f32,
}

impl Scorer {
    /// From the model folder (`content_keywords::model_folder()`).
    pub fn load(dir: &Path) -> Result<Self, String> {
        let text = TextTower::load(dir)?;
        let prompts: Vec<String> = TERMS.iter().map(|t| format!("a photo of {t}.")).collect();
        let terms = text.embed(&prompts.iter().map(String::as_str).collect::<Vec<_>>())?;
        let (scale, _bias) = logit_params(dir)?;
        Ok(Self { image: ImageTower::load(dir)?, terms, scale })
    }

    /// Share of the sky descriptions (0..1) for a picture.
    pub fn sky(&self, img: &image::RgbImage) -> f32 {
        let v = self.image.embed(&[img]).pop().unwrap_or_default();
        let logits: Vec<f32> = self.terms.iter().map(|t| self.scale * t.iter().zip(&v).map(|(a, b)| a * b).sum::<f32>()).collect();
        share(&logits, SKY)
    }
}

/// Softmax share of the first `n` logits.
pub fn share(logits: &[f32], n: usize) -> f32 {
    let m = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let e: Vec<f32> = logits.iter().map(|l| (l - m).exp()).collect();
    let all: f32 = e.iter().sum();
    if all > 0.0 { e[..n.min(e.len())].iter().sum::<f32>() / all } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_of_the_first_logits() {
        assert!((share(&[0.0, 0.0], 1) - 0.5).abs() < 1e-6);
        assert!(share(&[10.0, 0.0, 0.0], 1) > 0.99);
        assert!(share(&[0.0, 10.0], 1) < 0.01);
    }

    /// With the model (installed or `GRAINEO_HF_CACHE`): a blue gradient counts as sky, a grey checkerboard not.
    #[test]
    fn blue_gradient_is_sky() {
        let Some(dir) = crate::siglip::folder(crate::content_keywords::MODEL.id, crate::content_keywords::MODEL.repo) else { return };
        let s = Scorer::load(&dir).unwrap();
        let sky = image::RgbImage::from_fn(256, 256, |_, y| image::Rgb([(90 + y / 4) as u8, (140 + y / 5) as u8, 230]));
        let board = image::RgbImage::from_fn(256, 256, |x, y| if (x / 32 + y / 32) % 2 == 0 { image::Rgb([60, 60, 60]) } else { image::Rgb([200, 200, 200]) });
        let (a, b) = (s.sky(&sky), s.sky(&board));
        eprintln!("sky {a:.3}, board {b:.3}");
        assert!(a > 0.5 && b < a);
    }
}
