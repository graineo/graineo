//! Local Lightroom catalog settings → editable mask document.
//! Own, limited interpretation of the user's settings; no Adobe profile data.
//! Parts that are not understood are counted. With unknown mask geometry the
//! whole adjustment is skipped, so that e.g. a missing AI selection does not act globally.
use crate::lightroom::{Value, Values};
use develop::settings::MaskImportReport;
use masks::{Adjustments, Document, Layer, Mask, Operation};

type Result<T> = std::result::Result<T, String>;
const MODERN: &str = "MaskGroupBasedCorrections";
const LEGACY: [&str; 3] = [
    "GradientBasedCorrections",
    "CircularGradientBasedCorrections",
    "PaintBasedCorrections",
];

fn number(w: &Values, key: &str, default: f32) -> Result<f32> {
    match w.get(key) {
        None => Ok(default),
        Some(v) => v
            .number()
            .filter(|n| n.is_finite() && n.abs() <= f32::MAX as f64)
            .map(|n| n as f32)
            .ok_or_else(|| key.into()),
    }
}
fn required(w: &Values, key: &str) -> Result<f32> {
    if !w.contains_key(key) {
        return Err(key.into());
    }
    number(w, key, 0.0)
}
fn boolean(w: &Values, key: &str, default: bool) -> Result<bool> {
    match w.get(key) {
        None => Ok(default),
        Some(v) => v.flag().ok_or_else(|| key.into()),
    }
}
fn list<'a>(w: &'a Values, key: &str) -> Result<&'a [Value]> {
    match w.get(key) {
        Some(Value::List(v)) => Ok(v),
        _ => Err(key.into()),
    }
}
fn table(v: &Value) -> Result<&Values> {
    match v {
        Value::Table(w) => Ok(w),
        _ => Err("Mask structure".into()),
    }
}
fn note(r: &mut MaskImportReport, key: impl Into<String>) {
    *r.unsupported.entry(key.into()).or_default() += 1;
}
fn join(mut masks: Vec<Mask>) -> Result<Mask> {
    // Addition is associative; a balanced tree keeps even long lists flat.
    if masks.is_empty() {
        return Err("Empty mask".into());
    }
    while masks.len() > 1 {
        let mut next = Vec::with_capacity(masks.len().div_ceil(2));
        let mut it = masks.into_iter();
        while let Some(a) = it.next() {
            next.push(match it.next() {
                Some(b) => Mask::Combine {
                    operation: Operation::Add,
                    left: Box::new(a),
                    right: Box::new(b),
                },
                None => a,
            });
        }
        masks = next;
    }
    Ok(masks.pop().unwrap())
}
struct Reader {
    legacy: bool,
    scale: [f32; 2],
    nodes: usize,
    points: usize,
}
impl Reader {
    fn mask(&mut self, w: &Values, depth: usize) -> Result<Option<Mask>> {
        self.nodes += 1;
        if depth > 24 || self.nodes > 2048 {
            return Err("Mask limits".into());
        }
        if !boolean(w, "MaskActive", true)? {
            return Ok(None);
        }
        if number(w, "MaskBlendMode", 0.0)? != 0.0 {
            return Err("MaskBlendMode".into());
        }
        // the value a mask part paints with: for a brush stroke the density (its highest weight, read as such: an own
        // interpretation, not checked against Lightroom's rendering); 0 is an erasing stroke, which is not taken over
        let value = number(w, "MaskValue", 1.0)?;
        let kind = w.get("What").and_then(Value::text).ok_or("Mask type")?;
        if value != 1.0 && !(kind == "Mask/Paint" && value > 0.0 && value < 1.0) {
            return Err("MaskValue".into());
        }
        let mut m = match kind {
            "Mask/Aggregate" => {
                let masks = self.masks(list(w, "Masks")?, depth + 1)?;
                if masks.is_empty() {
                    return Ok(None);
                }
                join(masks)?
            }
            "Mask/Gradient" => Mask::Linear {
                start: [required(w, "ZeroX")?, required(w, "ZeroY")?],
                end: [required(w, "FullX")?, required(w, "FullY")?],
            },
            "Mask/CircularGradient" => {
                // The measured examples have neither rotation nor a modified shape curve.
                // Adopt other angle/shape conventions only after a cross-check.
                for (key, neutral) in [("Angle", 0.0), ("Midpoint", 50.0), ("Roundness", 0.0)] {
                    if number(w, key, neutral)? != neutral {
                        return Err(key.into());
                    }
                }
                let [l, t, r, b] = [
                    required(w, "Left")?,
                    required(w, "Top")?,
                    required(w, "Right")?,
                    required(w, "Bottom")?,
                ];
                if r <= l || b <= t {
                    return Err("Radial bounds".into());
                }
                let radial = Mask::Radial {
                    center: [(l + r) * 0.5, (t + b) * 0.5],
                    radii: [(r - l) * 0.5 * self.scale[0], (b - t) * 0.5 * self.scale[1]],
                    angle: 0.0,
                    feather: number(w, "Feather", 50.0)? / 100.0,
                };
                // Converted old radial filters keep Flipped (inside selection).
                // MaskInverted is an additional, independent invert switch.
                // Modern radial masks without Flipped start with the inside selection.
                if (self.legacy || w.contains_key("Flipped")) && !boolean(w, "Flipped", false)? {
                    Mask::Invert {
                        mask: Box::new(radial),
                    }
                } else {
                    radial
                }
            }
            "Mask/Paint" => {
                if boolean(w, "AutoMask", false)? {
                    return Err("AutoMask".into());
                }
                let dabs = list(w, "Dabs")?;
                self.points = self.points.checked_add(dabs.len()).ok_or("Dabs limits")?;
                if self.points > 100_000 {
                    return Err("Dabs limits".into());
                }
                let mut points = Vec::with_capacity(dabs.len());
                for dab in dabs {
                    let s = dab.text().ok_or("Dabs format")?;
                    let mut words = s.split_whitespace();
                    if words.next() != Some("d") {
                        return Err("Dabs format".into());
                    }
                    let mut next = || {
                        words
                            .next()
                            .and_then(|v| v.parse::<f32>().ok())
                            .filter(|v| v.is_finite())
                            .ok_or("Dabs coordinates")
                    };
                    points.push([next()?, next()?]);
                    if words.next().is_some() {
                        return Err("Dabs format".into());
                    }
                }
                let radius = required(w, "Radius")?;
                // The intermediate dabs inserted by the compute kernel must also fit its
                // budget; otherwise only the image computation would reject the whole layer.
                let mut length = 0.0;
                for pair in points.windows(2) {
                    length += ((pair[1][0] - pair[0][0]) * self.scale[0])
                        .hypot((pair[1][1] - pair[0][1]) * self.scale[1]);
                }
                if radius <= 0.0 || !length.is_finite() || length / (radius * 0.25) > (masks::DABS_MAX - 1) as f32 {
                    return Err("Dabs limits".into());
                }
                Mask::Brush {
                    points,
                    radius,
                    hardness: number(w, "CenterWeight", 0.0)?,
                    flow: number(w, "Flow", 1.0)?,
                    density: value,
                    auto: None,
                }
            }
            // Never replace AI, range masks and new formats with a full mask.
            _ => return Err(kind.chars().take(100).collect()),
        };
        if boolean(w, "MaskInverted", false)? {
            m = Mask::Invert { mask: Box::new(m) };
        }
        m.validate().map_err(|_| format!("Invalid {kind}"))?;
        Ok(Some(m))
    }
    fn masks(&mut self, values: &[Value], depth: usize) -> Result<Vec<Mask>> {
        if values.len() > 2048 {
            return Err("Mask limits".into());
        }
        let mut out = Vec::new();
        for v in values {
            if let Some(m) = self.mask(table(v)?, depth)? {
                out.push(m);
            }
        }
        Ok(out)
    }
}
fn adjustments(w: &Values, amount: f32, report: &mut MaskImportReport) -> Result<Adjustments> {
    let mut a = Adjustments::default();
    for (key, value) in w.iter().filter(|(k, _)| k.starts_with("Local")) {
        // PV2012 takes precedence over old compatibility values stored at the same time.
        // HashMap order must not affect the result, even with neutral PV2012.
        if matches!(
            key.as_str(),
            "LocalExposure" | "LocalContrast" | "LocalClarity"
        ) && w.contains_key(&format!("{key}2012"))
        {
            continue;
        }
        let neutral = if key == "LocalCurveRefineSaturation" {
            100.0
        } else {
            0.0
        };
        let Some(n) = value.number().filter(|v| v.is_finite()) else {
            note(report, key);
            continue;
        };
        if n == neutral {
            continue;
        }
        // Local sliders in the catalog are normalized, not the global 0…100 values.
        // Exposure ±1 corresponds here approximately to the range ±4 EV.
        let field = match key.as_str() {
            "LocalExposure2012" | "LocalExposure" => {
                if n.abs() > 1.0 {
                    return Err(key.clone());
                }
                a.exposure = n as f32 * 4.0 * amount;
                continue;
            }
            "LocalContrast2012" | "LocalContrast" => &mut a.contrast,
            "LocalHighlights2012" => &mut a.highlights,
            "LocalShadows2012" => &mut a.shadows,
            "LocalTemperature" => &mut a.temperature,
            "LocalTint" => &mut a.tint,
            "LocalSaturation" => &mut a.saturation,
            "LocalClarity2012" | "LocalClarity" => &mut a.clarity,
            "LocalTexture" => &mut a.texture,
            "LocalSharpness" if n >= 0.0 => &mut a.sharpness,
            "LocalLuminanceNoise" if n >= 0.0 => &mut a.noise_reduction,
            _ => {
                note(report, key);
                continue;
            }
        };
        *field = n as f32 * amount;
    }
    a.validate()
        .map_err(|_| "Local adjustment bounds".to_string())?;
    Ok(a)
}

/// Sizes refer to the unrotated, uncropped image. A source with a
/// modern, non-empty mask list takes precedence over its old compatibility lists.
/// No file/database access. Existing graineo masks are preserved by the caller.
pub fn import(w: &Values, size: [usize; 2]) -> (Option<Document>, Option<MaskImportReport>) {
    let modern = match w.get(MODERN) {
        Some(Value::List(v)) => !v.is_empty(),
        Some(_) => true,
        None => false,
    };
    let keys: &[&str] = if modern { &[MODERN] } else { &LEGACY };
    let mut report = MaskImportReport {
        version: 1,
        ..Default::default()
    };
    let mut doc = Document {
        version: 2,
        layers: Vec::new(),
    };
    let min = size[0].min(size[1]).max(1) as f32;
    let mut reader = Reader {
        legacy: false,
        scale: [size[0].max(1) as f32 / min, size[1].max(1) as f32 / min],
        nodes: 0,
        points: 0,
    };
    for &key in keys {
        reader.legacy = key != MODERN;
        let Some(value) = w.get(key) else { continue };
        let Value::List(values) = value else {
            note(&mut report, key);
            continue;
        };
        report.total += values.len();
        if !w
            .get(&format!("Enable{key}"))
            .and_then(Value::flag)
            .unwrap_or(true)
        {
            report.skipped += values.len();
            continue;
        }
        for v in values.iter().take(64) {
            let result = (|| -> Result<Option<Layer>> {
                if doc.layers.len() >= 64 {
                    return Err("Layer limits".into());
                }
                let c = table(v)?;
                if !boolean(c, "CorrectionActive", true)? {
                    return Ok(None);
                }
                let amount = number(c, "CorrectionAmount", 1.0)?;
                if !(0.0..=1.0).contains(&amount) {
                    return Err("CorrectionAmount".into());
                }
                if amount == 0.0 {
                    return Ok(None);
                }
                if let Some(range) = c.get("CorrectionRangeMask") {
                    if number(table(range)?, "Type", 0.0)? != 0.0 {
                        return Err("CorrectionRangeMask".into());
                    }
                }
                let masks = reader.masks(list(c, "CorrectionMasks")?, 0)?;
                if masks.is_empty() {
                    return Ok(None);
                }
                let mask = join(masks)?;
                mask.validate().map_err(|_| "Mask limits".to_string())?;
                let adjustments = adjustments(c, amount, &mut report)?;
                Ok(Some(Layer::new(mask, adjustments)))
            })();
            match result {
                Ok(Some(layer)) => {
                    doc.layers.push(layer);
                    report.imported += 1;
                }
                Ok(None) => report.skipped += 1,
                Err(key) => {
                    report.skipped += 1;
                    note(&mut report, key);
                }
            }
        }
        let excess = values.len().saturating_sub(64);
        if excess != 0 {
            report.skipped += excess;
            *report.unsupported.entry("Layer limits".into()).or_default() += excess;
        }
    }
    let any = report.total != 0 || !report.unsupported.is_empty();
    let doc = (!doc.layers.is_empty()).then_some(doc);
    (doc, any.then_some(report))
}

/// Keys of [`not_taken`] (English; the app shows them translated).
pub const AI_MASKS: &str = "ai_masks";
pub const RANGE_MASKS: &str = "range_masks";
pub const SUBTRACTIONS: &str = "subtractions";
pub const OTHER_MASKS: &str = "other_masks";
pub const POINT_COLOR: &str = "point_color";
pub const REMOVE_SPOTS: &str = "remove_spots";

/// `PointColors = { … }` with at least one entry (Lightroom 15 writes an empty one into every text).
fn point_colors_listed(text: &str) -> bool {
    text.match_indices("PointColors").any(|(i, k)| {
        let rest = text[i + k.len()..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else { return false };
        let Some(rest) = rest.trim_start().strip_prefix('{') else { return false };
        !rest.trim_start().starts_with('}')
    })
}

/// What graineo does not take over from one Lightroom settings text (`Adobe_imageDevelopSettings.text`), counted per
/// kind: local corrections whose masks cannot be read (AI selections `Mask/Image`, range masks, subtracted parts,
/// others – one count per correction, by the first reason, as [`import`] skips them), point color entries and
/// spots of the newer remove tool (`RetouchAreas`; the older spot removal `RetouchInfo` is taken over). Empty if
/// everything comes along. Quick for texts without any of these (no parsing).
pub fn not_taken(text: &str) -> std::collections::BTreeMap<String, usize> {
    let mut out = std::collections::BTreeMap::new();
    let masks = text.contains("CorrectionMasks");
    let remove = text.contains("RetouchAreas");
    let point = point_colors_listed(text);
    if !(masks || remove || point) {
        return out;
    }
    let Some(w) = crate::lightroom::from_catalog(text) else { return out };
    let len = |key: &str| match w.get(key) {
        Some(Value::List(v)) => v.len(),
        Some(Value::Table(t)) => t.len(),
        _ => 0,
    };
    if masks {
        if let (_, Some(r)) = import(&w, [1, 1]) {
            for (reason, n) in r.unsupported {
                let key = match reason.as_str() {
                    "Mask/Image" => AI_MASKS,
                    "Mask/RangeMask" | "CorrectionRangeMask" => RANGE_MASKS,
                    "MaskBlendMode" => SUBTRACTIONS,
                    _ => OTHER_MASKS,
                };
                *out.entry(key.to_string()).or_default() += n;
            }
        }
    }
    for (key, n) in [(POINT_COLOR, len("PointColors")), (REMOVE_SPOTS, len("RetouchAreas"))] {
        if n > 0 {
            *out.entry(key.to_string()).or_default() += n;
        }
    }
    out
}

#[cfg(test)]
#[path = "lr_masks_tests.rs"]
mod tests;
