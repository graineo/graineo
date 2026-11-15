//! Pipeline inspection ("for Foveon nerds"): intermediate development stages as images and
//! difference images showing what a stage changed. Everything at preview size and only from the
//! preview data that exists anyway (sensor preview, own development preview,
//! downscaled SPP base); no new background load, no session lock.

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use x3f::RawImage;

use crate::matrix::Mat3;
use crate::session::Preview;
use crate::{calib, Calibration, EncodeLut, Linear, Rgb16, Session, Settings, View};

/// One development stage in processing order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Stage {
    /// Counts as read (defective pixels replaced), without black subtraction and flat field
    Raw,
    /// Black level, spatial gain (flat field) and colour shading
    Preprocessed,
    /// White balance and colour matrix (own computation, without noise reduction), linear
    Matrix,
    /// Sigma's base: noise reduction + colour matrix + ColorDQ, linear before the tone curve
    SppBase,
    /// Sigma's sharpness slider on the base, linear
    Sharpen,
    /// Sigma tone curve, gamma: finished without look
    Tone,
    /// Finished with look (tone curve, colour mixer, color grading, effects, fringes)
    Final,
}

impl Stage {
    pub const ALL: [Stage; 7] = [Stage::Raw, Stage::Preprocessed, Stage::Matrix, Stage::SppBase, Stage::Sharpen, Stage::Tone, Stage::Final];

    pub fn label(self) -> &'static str {
        match self {
            Stage::Raw => "1 Rohdaten",
            Stage::Preprocessed => "2 Vorverarbeitung",
            Stage::Matrix => "3 Farbmatrix (ohne Rauschunterdr.)",
            Stage::SppBase => "4 Rauschunterdrückung (Foveon-Basis)",
            Stage::Sharpen => "5 Foveon-Schärfe",
            Stage::Tone => "6 Tonkurve (ohne Look)",
            Stage::Final => "7 Fertig mit Look",
        }
    }

    /// Short explanation line.
    pub fn explain(self) -> &'static str {
        match self {
            Stage::Raw => "Zählwerte der drei Schichten wie gelesen (unten → Rot, Mitte → Grün, oben → Blau), Defektpixel ersetzt – noch ohne Schwarzabzug und Flat-Field.",
            Stage::Preprocessed => "Schwarzwert abgezogen, Spatial Gain (Flat-Field) und Color Shading ausgeglichen. Noch Sensorfarben, keine Matrix.",
            Stage::Matrix => "Weißabgleich, Farbmatrix und ColorDQ auf die Sensorwerte – ohne Rauschunterdrückung, linear (ohne Tonkurve).",
            Stage::SppBase => "Sigmas Arbeitsbild: Rauschunterdrückung, Farbmatrix und ColorDQ in einem Schritt (enthält Sigmas Grundschärfung), linear vor der Tonkurve.",
            Stage::Sharpen => "Sigmas Schärfe-Regler auf der Basis: nur die Helligkeit ändert sich um das Detailbild L, die Farbverhältnisse bleiben.",
            Stage::Tone => "Sigma-Tonkurve mit Belichtung, Kontrast, Schatten, Lichtern, Fill Light und Sättigung – fertig, aber ohne Look-Werkzeuge.",
            Stage::Final => "Mit Gradationskurve, Farbmischer, Color Grading, Klarheit, Vignette, Korn und Farbsaum-Entfernung – das entwickelte Bild.",
        }
    }

    /// What the difference image of this stage shows (`None`: first stage, no difference).
    pub fn diff_label(self) -> Option<&'static str> {
        Some(match self {
            Stage::Raw => return None,
            Stage::Preprocessed => "Korrekturkarte: Spatial Gain × Color Shading je Schicht (grau = × 1, heller = angehoben). Der Schwarzwert wird zusätzlich gleichmäßig abgezogen.",
            Stage::Matrix => "Differenz zu 2: was Weißabgleich und Farbmatrix an Farbe und Helligkeit verschieben.",
            Stage::SppBase => "Differenz zu 3: das entfernte Rauschen (dazu die Grundschärfung der Basis).",
            Stage::Sharpen => "Detailbild L: der Hochpass, mit dem Sigmas Schärfe-Regler die Helligkeit verändert.",
            // Tone curve vs. linear display: almost only brightness, not informative as a difference
            Stage::Tone => return None,
            Stage::Final => "Differenz zu 6: was die Look-Werkzeuge verändert haben (grau = nichts).",
        })
    }

    fn prev(self) -> Option<Stage> {
        let i = Stage::ALL.iter().position(|&s| s == self)?;
        i.checked_sub(1).map(|i| Stage::ALL[i])
    }
}

/// Note on the accuracy of a stage (text, SPP-exact?) for the UI.
/// `has_spp`: SPP base is available; `prepared`: Quattro/Classic via x3fuse-core.
pub fn exactness(stage: Stage, has_spp: bool, prepared: bool) -> (&'static str, bool) {
    match stage {
        Stage::Raw => ("Exakt: dekodierte Rohdaten (Vorschau: Blockmittel).", true),
        Stage::Preprocessed => ("Eigene Rechnung mit Sigmas Kalibrierung – in der Foveon-Verarbeitung steckt dieser Schritt in der Basis und ist dort nicht einzeln abgreifbar.", false),
        Stage::Matrix => ("Eigene Pipeline (wie „ohne Rauschunterdrückung“), nicht in der Foveon-Verarbeitung.", false),
        Stage::SppBase | Stage::Sharpen | Stage::Tone if !has_spp => {
            ("Foveon-Basis liegt (noch) nicht vor – angezeigt wird die eigene Näherung.", false)
        }
        Stage::SppBase => ("Foveon-Verarbeitung (Vorschau verkleinert).", true),
        Stage::Sharpen if prepared => ("Sigmas Schärfe gibt es nur bei Merrill – hier gleich Stufe 4.", false),
        Stage::Sharpen => ("Foveon-Verarbeitung in 1:1; in der Vorschau Näherung des Detailbilds.", true),
        Stage::Tone => ("Foveon-Verarbeitung (Vorschau verkleinert), solange keine eigenen Regler wirken.", true),
        Stage::Final => ("Look-Werkzeuge sind eigene – die Foveon-Verarbeitung kennt sie nicht.", false),
    }
}

/// Preview data from which the stages are produced.
pub struct Sources<'a> {
    pub raw: &'a RawImage,
    /// Sensor preview (after preprocessing, without noise reduction)
    pub sensor: &'a Preview,
    /// Own development preview (own noise reduction)
    pub developed: &'a Preview,
    /// Downscaled SPP base, if computed
    pub spp: Option<&'a Preview>,
}

/// Renders the stage selected in `s.view` (or its difference image).
pub fn render(src: &Sources, cal: &Calibration, s: &Settings) -> Rgb16 {
    let dev = Settings { view: View::Developed, ..s.clone() };
    let View::Stage { stage, diff, gain } = s.view else {
        return src.developed.render(cal, &dev);
    };
    let ev = 2f32.powf(s.view_gain_ev);
    if !diff {
        return stage_image(stage, src, cal, &dev, ev);
    }
    let g = 2f32.powi(i32::from(gain.min(10)));
    if stage == Stage::Preprocessed {
        return correction_map(src.raw, cal, src.sensor.factor, g);
    }
    if stage == Stage::Sharpen {
        if let Some(img) = src.spp.and_then(|p| detail_image(&p.lin, cal, g)) {
            return img;
        }
    }
    match stage.prev() {
        Some(p) => difference(&stage_image(stage, src, cal, &dev, ev), &stage_image(p, src, cal, &dev, ev), g),
        None => stage_image(stage, src, cal, &dev, ev),
    }
}

fn stage_image(stage: Stage, src: &Sources, cal: &Calibration, s: &Settings, ev: f32) -> Rgb16 {
    let to_display = ev / cal.max_count();
    // Finished development as in the render job: SPP preview if the settings use it
    let finished = match src.spp {
        Some(p) if Session::uses_spp(s) => p,
        _ => src.developed,
    };
    match stage {
        Stage::Raw => show(&raw_scaled(src.raw, cal, src.sensor.factor), ev),
        Stage::Preprocessed => show(&src.sensor.lin, ev),
        Stage::Matrix => show(&own_counts(&src.sensor.lin, cal, s), to_display),
        Stage::SppBase | Stage::Sharpen => match src.spp {
            Some(p) => show(&spp_counts(p, cal, s, stage == Stage::Sharpen), to_display),
            None => show(&own_counts(&src.developed.lin, cal, s), to_display),
        },
        Stage::Tone => finished.render_base(cal, s),
        Stage::Final => finished.render(cal, s),
    }
}

/// Raw data of the active area downscaled by `factor` (block mean), divided by the
/// saturation; without black subtraction, spatial gain and colour shading.
pub fn raw_scaled(raw: &RawImage, cal: &Calibration, factor: usize) -> Linear {
    let f = factor.max(1);
    let [x0, y0, x1, y1] = cal.active;
    let (width, height) = ((x1 - x0 + 1) / f, (y1 - y0 + 1) / f);
    let norm = 1.0 / (cal.saturation as f32 * (f * f) as f32);
    let mut data = vec![0f32; width * height * 3];
    data.par_chunks_mut(width * 3).enumerate().for_each(|(y, line)| {
        let mut acc = vec![[0u32; 3]; width];
        for sy in y0 + y * f..y0 + (y + 1) * f {
            let base = sy * raw.columns + x0;
            for (c, plane) in raw.planes.iter().enumerate() {
                for (x, block) in plane[base..base + width * f].chunks_exact(f).enumerate() {
                    acc[x][c] += block.iter().map(|&v| u32::from(v)).sum::<u32>();
                }
            }
        }
        for (x, a) in acc.iter().enumerate() {
            for c in 0..3 {
                line[x * 3 + c] = a[c] as f32 * norm;
            }
        }
    });
    Linear { mono: false, haze: None, toneq: None, width, height, data, detail: None }
}

/// Flat-field correction per layer (spatial gain × colour shading) as in
/// [`crate::linearize_scaled`], evaluated at the block centres: grey = × 1, (f − 1) · `gain` / 8.
fn correction_map(raw: &RawImage, cal: &Calibration, factor: usize, gain: f32) -> Rgb16 {
    let f = factor.max(1);
    let [x0, y0, x1, y1] = cal.active;
    let (width, height) = ((x1 - x0 + 1) / f, (y1 - y0 + 1) / f);
    let k = gain / 8.0 * 65535.0;
    let mut data = vec![0u16; width * height * 3];
    data.par_chunks_mut(width * 3).enumerate().for_each(|(y, line)| {
        let cy = y0 + y * f + f / 2;
        let rows: Option<[Vec<f32>; 3]> = cal.spatial.as_ref().map(|t| std::array::from_fn(|c| t[c].row(cy, raw.rows)));
        for x in 0..width {
            let cx = x0 + x * f + f / 2;
            let (sr, sb) = cal.shading.map_or((1.0, 1.0), |s| s.factors(cx, cy));
            let shade = [sr, 1.0, sb];
            for c in 0..3 {
                let g = match (&cal.spatial, &rows) {
                    (Some(t), Some(r)) => t[c].at_row(&r[c], cx, raw.columns),
                    _ => 1.0,
                };
                line[x * 3 + c] = (32768.0 + (g * shade[c] - 1.0) * k).clamp(0.0, 65535.0) as u16;
            }
        }
    });
    Rgb16 { width, height, data }
}

/// Own pipeline up to the tone curve (like [`crate::render`] without Fill Light, presence and
/// highlight neutralisation): sensor values → counts in the output space.
fn own_counts(lin: &Linear, cal: &Calibration, s: &Settings) -> Linear {
    let sat = cal.saturation as f32;
    let ratio = cal.wb_ratio(s);
    let gain: [f32; 3] = std::array::from_fn(|c| (cal.count_scale * cal.wb_gain[c] * ratio[c]) as f32 * sat);
    let m = Mat3::diag(calib::temp_tint_gains(s.wb_temp, s.wb_tint)).mul(&cal.cam_to_srgb(s.color_mode.as_deref(), s.white_balance.as_deref())).to_f32();
    let dq = cal.color_dq.as_ref();
    let mut data = lin.data.clone();
    data.par_chunks_mut(3).for_each(|p| {
        let v = [p[0] * gain[0], p[1] * gain[1], p[2] * gain[2]];
        let mut rgb = [
            m[0] * v[0] + m[1] * v[1] + m[2] * v[2],
            m[3] * v[0] + m[4] * v[1] + m[5] * v[2],
            m[6] * v[0] + m[7] * v[1] + m[8] * v[2],
        ];
        if let Some(dq) = dq {
            rgb = dq.apply(rgb);
        }
        p.copy_from_slice(&rgb);
    });
    Linear { mono: false, haze: None, toneq: None, width: lin.width, height: lin.height, data, detail: None }
}

/// SPP counts of the preview, converted to the colour mode/white balance of the settings as in
/// [`crate::render_counts`]; with `sharpen` additionally Sigma's sharpness slider.
fn spp_counts(p: &Preview, cal: &Calibration, s: &Settings, sharpen: bool) -> Linear {
    let counts = &p.lin;
    let remap = p.spp.as_ref().and_then(|(_, mode_matrix)| {
        let inv = mode_matrix.inverse().unwrap_or(Mat3::IDENTITY);
        let target = cal.output_matrix(s).mul(&inv);
        target.0.iter().zip(Mat3::IDENTITY.0.iter()).any(|(a, b)| (a - b).abs() > 1e-9).then(|| target.to_f32())
    });
    let sharp = counts
        .detail
        .as_deref()
        .filter(|d| sharpen && d.l.len() == counts.width * counts.height)
        .and_then(|d| Some((d, d.amounts(s.sigma_sharpness)?)));
    let sharp_own = sharp.is_some() && x3f_core::merrill_new::active("sharpness");
    let mut data = counts.data.clone();
    data.par_chunks_mut(3).enumerate().for_each(|(i, px)| {
        let mut q = [px[0], px[1], px[2]];
        if let Some((d, (amt0, amt))) = sharp {
            q = if sharp_own { crate::sharpness::own_sharpen_px(q, d.l[i], amt - amt0) } else { crate::sharpness::resharpen_px(q, d.l[i], amt0, amt) };
        }
        if let Some(m) = &remap {
            q = [
                m[0] * q[0] + m[1] * q[1] + m[2] * q[2],
                m[3] * q[0] + m[4] * q[1] + m[5] * q[2],
                m[6] * q[0] + m[7] * q[1] + m[8] * q[2],
            ];
        }
        px.copy_from_slice(&q);
    });
    Linear { mono: false, haze: None, toneq: None, width: counts.width, height: counts.height, data, detail: None }
}

/// Linear values × `scale` with sRGB gamma for display.
fn show(lin: &Linear, scale: f32) -> Rgb16 {
    let enc = EncodeLut::new();
    let data = lin.data.par_iter().map(|&v| enc.get(v * scale)).collect();
    Rgb16 { width: lin.width, height: lin.height, data }
}

/// Signed difference of two display images: grey = equal, brighter/darker or
/// coloured = change, amplified by `gain`. Different sizes: common area.
fn difference(a: &Rgb16, b: &Rgb16, gain: f32) -> Rgb16 {
    let (w, h) = (a.width.min(b.width), a.height.min(b.height));
    let mut data = vec![0u16; w * h * 3];
    data.par_chunks_mut(w * 3).enumerate().for_each(|(y, line)| {
        let ra = &a.data[y * a.width * 3..(y * a.width + w) * 3];
        let rb = &b.data[y * b.width * 3..(y * b.width + w) * 3];
        for ((o, &va), &vb) in line.iter_mut().zip(ra).zip(rb) {
            let d = (f32::from(va) - f32::from(vb)) * gain;
            *o = (32768.0 + d).clamp(0.0, 65535.0) as u16;
        }
    });
    Rgb16 { width: w, height: h, data }
}

/// Sigma's detail image L as grey around the middle (`None` without a detail image, e.g. Quattro).
fn detail_image(lin: &Linear, cal: &Calibration, gain: f32) -> Option<Rgb16> {
    let d = lin.detail.as_deref().filter(|d| d.l.len() == lin.width * lin.height)?;
    // L is fixed point ×4: sharpness changes the brightness R+G+B by 0.25 · L · amount (counts)
    let k = gain * 0.25 * 65535.0 / cal.max_count();
    let data = d.l.par_iter().flat_map_iter(|&l| std::iter::repeat_n((32768.0 + f32::from(l) * k).clamp(0.0, 65535.0) as u16, 3)).collect();
    Some(Rgb16 { width: lin.width, height: lin.height, data })
}
