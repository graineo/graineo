//! Own lens profiles of graineo: file format (2, reads 1), storage with versions and history, lens model
//! and its interpolation, colour-cast maps per channel/layer, flat-field measurement and Lanczos sampling.
//!
//! Leaf crate (only `serde`, `serde_json`, `sysdirs`): `raw` (Bayer/X-Trans) and later `develop` (X3F)
//! depend on it.

pub mod assign;
pub mod cast;
pub mod exchange;
pub mod flatfield;
pub mod history;
pub mod model;
pub mod names;
pub mod own;
pub mod quality;
pub mod resample;
pub mod store;

pub use own::{OwnProfile, ProfileRef};
