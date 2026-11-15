//! Non-Sigma RAWs (CR2/CR3/RAF/DNG/ARW …) in the Develop module: preview with caches,
//! sliders on the linear chain, export, starting values from Lightroom.
//!
//! Image quality (demosaicing, highlights, color, noise, lens, looks) comes from `crates/raw`;
//! the look stage (curve, color mixer, clarity, color grading, effects), film and cropping
//! from `crates/develop` – this crate only holds the glue and the sliders that exist only for RAWs.

pub mod doc;
pub mod export;
pub mod false_color;
pub mod calibration;
pub mod gpu;
pub mod lens_shots;
pub mod look_lut;
pub mod lightroom;
pub mod lr_masks;
pub mod render;
pub mod wb;
pub mod timings;

pub use doc::{is_editable, is_image, is_raw, RawDoc, Stage};

pub mod film_profiles;

pub mod color;
