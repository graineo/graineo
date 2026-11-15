//! Classic: conversion between the camera values of the shared pipeline (white-balanced raw values,
//! 12-bit units) and the linear working codes of the chain, per colour mode.
//!
//! The chain's codes are a matrix image of the layers: camera matrix of the white balance (stored
//! correction × gains, or correction × illuminant), scaled to the neutral point, times the colour
//! mode matrix, at high ISO with automatic or incandescent white balance mixed with the daylight
//! matrix, mapped to RIMM with the ISO factor ([`calibration::cam2rimm`]). The pipeline turns the
//! chain's codes into camera values with the inverse of the shot mode's matrix and back with the
//! matrix of the chosen mode; the output stage ([`super::output`]) then works on the codes again.

use x3f::{Camf, PropList};

use super::calibration::{self, mm, Data};
use crate::matrix::Mat3;

/// Conversion of one colour mode.
#[derive(Clone, Debug)]
pub struct Conversion {
    /// camera (12-bit units, 1.0 = 4096 codes) → linear working codes / 4096
    pub a: [f32; 9],
    /// camera counts (shot white balance) → 12-bit units: divided by `count_scale · wb_gain ·
    /// saturation` when used
    pub k_raw: [f64; 3],
    /// white balance gain of the calibration when `k_raw` was computed
    pub wb_gain: [f64; 3],
    /// raw units → 12-bit units (temperature and aperture gain, neutral point, linear-table factor)
    raw_scale: [f64; 3],
    /// 12-bit units per raw unit at gain 1
    k0: f64,
    /// incandescent light from ISO 400 on: weight of the stored gains against the image's grey world
    pub gray_mix: Option<f64>,
    /// only usable with the chain's result (cameras without the white balance tables, SD9/SD10/SD14)
    pub only_chain: bool,
}

impl Conversion {
    /// Conversion for colour mode matrix `mode` at white balance `wb`, ISO factor `iso_factor`
    /// (capture ISO / sensor ISO) and the pipeline's white balance gain. The matrix and the layer gains
    /// come from the chain's calibration ([`calibration::calc`]).
    pub fn new(camf: &Camf, props: &PropList, adobe: bool, wb: &str, iso_factor: f64, wb_gain: [f64; 3], mode: &Mat3) -> Option<Self> {
        let k = calibration::calc(&calibration::Input { camf, props, wb, adobe, auto_neutral: None, gray_mean: None }).ok()?;
        let d = Data { camf };
        // camera matrix after the neutral point's scaling, times the colour mode matrix; at high ISO
        // with automatic or incandescent white balance mixed with the daylight matrix
        let mut m2 = mm(&k.color.m_single, &mode.0);
        if k.iso.high && (k.wb.code == 1 || k.wb.code == 5) {
            let tag = if k.color.cached { k.color.m2? } else { super::region::daylight_simple(&d, k.wb.name).ok()? };
            let w = if d.present("MatrixMixing") { f64::from(d.number("MatrixMixing")) } else { 0.5 };
            m2 = std::array::from_fn(|i| w * m2[i] + (1.0 - w) * tag[i]);
        }
        if k.skip & 0x40000 != 0 {
            m2 = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
        }
        let a = calibration::cam2rimm(&m2, &[1.0; 3], iso_factor, 0.0);
        // layer gain (neutral point, aperture, shading, temperature) and the input factor
        let linlut = match d.number("LinLUT_ReductionFactor") { 0.0 => 1.0, v => f64::from(v) };
        let k0 = (if k.color.cached { 1.3 } else { 1.0 }) * linlut / 4096.0;
        let raw_scale: [f64; 3] = std::array::from_fn(|c| k.amplification.gain[c] * k0);
        // the pipeline normalises raw values to its own saturation level (`RawSaturationLevel`, older
        // calibration: `SaturationLevel`)
        let saturation = if d.present("RawSaturationLevel") { d.triple("RawSaturationLevel", [0.0; 3]) } else { d.triple("SaturationLevel", [0.0; 3]) };
        let k_raw = std::array::from_fn(|c| saturation[c] * raw_scale[c]);
        let gray_mix = (k.iso.high && k.wb.code == 5).then(|| if d.present("WBMixing") { f64::from(d.number("WBMixing")) } else { 0.5 });
        Some(Self { a: a.map(|v| v as f32), k_raw, wb_gain, raw_scale, k0, gray_mix, only_chain: !k.color.cached })
    }

    /// Incandescent light from ISO 400 on: the layer gains mixed with the grey world of the image
    /// (`mean`: the chain's mean per layer), `g' = g · w + (mean₁/mean_c) · (1 − w)`.
    pub fn gray_world(&mut self, mean: [f64; 3]) {
        let Some(w) = self.gray_mix else { return };
        if mean.iter().any(|v| !(*v > 0.0)) {
            return;
        }
        let target = [mean[1] / mean[0], 1.0, mean[1] / mean[2]];
        for c in 0..3 {
            let g = self.raw_scale[c] / self.k0;
            let new = (g * w + target[c] * (1.0 - w)) * self.k0;
            // the scale to raw units (saturation level or measured relation) stays
            self.k_raw[c] *= new / self.raw_scale[c];
            self.raw_scale[c] = new;
        }
    }

    /// Relation of the pipeline's preprocessed layers to the raw values, measured on the image
    /// (`raw × spatial gain = slope · layer + offset`, see `quattro::classic_raw_map`); replaces the
    /// saturation level as the scale between the pipeline's values and raw units.
    pub fn raw_reference(&mut self, slope: [f64; 3]) {
        self.k_raw = std::array::from_fn(|c| slope[c] * self.raw_scale[c]);
    }
}
