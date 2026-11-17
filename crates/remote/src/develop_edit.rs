//! The simple sliders of the browser and their place in graineo's develop settings.
//!
//! The browser sees a small, flat set of values; they are written into a full
//! `develop::Settings` that starts from what the app would show on opening the photo, so
//! everything the browser does not know (crop, masks, film, lens corrections …) stays as it is.

use develop::Settings;
use serde_json::{json, Map, Value};

/// What kind of development the photo gets (decides ranges and white balance).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// Sigma X3F (Foveon): white balance as a relative fine adjustment, ranges of the Sigma panel
    Sigma,
    /// other RAW files: white balance in Kelvin and tint
    Raw,
    /// finished images (JPEG, HEIC, TIFF, PNG): relative white balance −100 … +100
    Finished,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Sigma => "sigma",
            Kind::Raw => "raw",
            Kind::Finished => "finished",
        }
    }
}

/// One slider: key, minimum, maximum, step (all in the units the browser shows).
pub struct Slider {
    pub key: &'static str,
    pub min: f64,
    pub max: f64,
    pub step: f64,
}

const fn s(key: &'static str, min: f64, max: f64, step: f64) -> Slider {
    Slider { key, min, max, step }
}

/// Sliders per kind, same ranges as the panels of the app.
pub fn sliders(kind: Kind) -> Vec<Slider> {
    match kind {
        Kind::Sigma => vec![
            s("temp", -1.0, 1.0, 0.01),
            s("tint", -1.0, 1.0, 0.01),
            s("exposure", -3.0, 3.0, 0.05),
            s("contrast", -2.0, 2.0, 0.05),
            s("highlights", -2.0, 2.0, 0.05),
            s("shadows", -2.0, 2.0, 0.05),
            s("clarity", -1.0, 1.0, 0.01),
            s("vibrance", -1.0, 1.0, 0.01),
            s("saturation", -1.0, 1.0, 0.01),
        ],
        Kind::Raw | Kind::Finished => {
            let (t, g) = if kind == Kind::Raw { (s("temp", 2000.0, 12000.0, 50.0), s("tint", -150.0, 150.0, 1.0)) } else { (s("temp", -100.0, 100.0, 1.0), s("tint", -100.0, 100.0, 1.0)) };
            vec![
                t,
                g,
                s("exposure", -5.0, 5.0, 0.01),
                s("contrast", -100.0, 100.0, 1.0),
                s("highlights", -100.0, 100.0, 1.0),
                s("shadows", -100.0, 100.0, 1.0),
                s("whites", -100.0, 100.0, 1.0),
                s("blacks", -100.0, 100.0, 1.0),
                s("clarity", -100.0, 100.0, 1.0),
                s("vibrance", -100.0, 100.0, 1.0),
                s("saturation", -100.0, 100.0, 1.0),
            ]
        }
    }
}

/// White balance presets of other RAW files (`raw.wb_mode`).
pub const RAW_WB_MODES: [&str; 9] = ["as_shot", "auto", "daylight", "cloudy", "shade", "tungsten", "fluorescent", "flash", "custom"];

fn r2(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

/// Values for the browser. `wb` = current (Kelvin, tint) of RAW files as the app computes them.
pub fn values(kind: Kind, st: &Settings, wb: Option<(f32, f32)>) -> Value {
    let mut m = Map::new();
    let pct = |v: f64| r2(v * 100.0);
    match kind {
        Kind::Sigma => {
            m.insert("temp".into(), json!(r2(st.wb_temp as f64)));
            m.insert("tint".into(), json!(r2(st.wb_tint as f64)));
            m.insert("exposure".into(), json!(r2(st.exposure)));
            m.insert("contrast".into(), json!(r2(st.contrast)));
            m.insert("highlights".into(), json!(r2(st.highlight)));
            m.insert("shadows".into(), json!(r2(st.shadow)));
            m.insert("clarity".into(), json!(r2(st.clarity as f64)));
            m.insert("vibrance".into(), json!(r2(st.vibrance as f64)));
            m.insert("saturation".into(), json!(r2(st.saturation as f64)));
        }
        Kind::Raw | Kind::Finished => {
            let (k, t) = wb.unwrap_or((st.raw.temperature, st.raw.tint));
            if kind == Kind::Finished {
                let (rt, rg) = rawdev::wb::relative_from(k, t);
                m.insert("temp".into(), json!(rt.round()));
                m.insert("tint".into(), json!(rg.round()));
            } else {
                m.insert("temp".into(), json!((k / 50.0).round() * 50.0));
                m.insert("tint".into(), json!(t.round()));
                m.insert("wb_mode".into(), json!(if st.raw.wb_mode.is_empty() { "as_shot" } else { st.raw.wb_mode.as_str() }));
            }
            m.insert("exposure".into(), json!(r2(st.exposure)));
            m.insert("contrast".into(), json!(pct(st.contrast)));
            m.insert("highlights".into(), json!(pct(st.highlight)));
            m.insert("shadows".into(), json!(pct(st.shadow)));
            m.insert("whites".into(), json!(pct(st.raw.whites as f64)));
            m.insert("blacks".into(), json!(pct(st.raw.blacks as f64)));
            m.insert("clarity".into(), json!(pct(st.clarity as f64)));
            m.insert("vibrance".into(), json!(pct(st.vibrance as f64)));
            m.insert("saturation".into(), json!(pct(st.saturation as f64)));
        }
    }
    m.insert("monochrome".into(), json!(st.monochrome));
    Value::Object(m)
}

/// Writes the browser's values into `st`. Unknown keys and values outside the ranges are
/// refused (no silent clamping of nonsense), missing keys keep their value.
pub fn apply(kind: Kind, st: &mut Settings, v: &Value, current_wb: Option<(f32, f32)>) -> Result<(), String> {
    let obj = v.as_object().ok_or("values must be an object")?;
    let list = sliders(kind);
    let num = |key: &str| -> Result<Option<f64>, String> {
        let Some(x) = obj.get(key) else { return Ok(None) };
        let x = x.as_f64().ok_or_else(|| format!("{key}: not a number"))?;
        let sl = list.iter().find(|s| s.key == key).ok_or_else(|| format!("{key}: not available"))?;
        if !x.is_finite() || x < sl.min - 1e-9 || x > sl.max + 1e-9 {
            return Err(format!("{key}: out of range"));
        }
        Ok(Some(x))
    };
    for k in obj.keys() {
        if !matches!(k.as_str(), "temp" | "tint" | "wb_mode" | "exposure" | "contrast" | "highlights" | "shadows" | "whites" | "blacks" | "clarity" | "vibrance" | "saturation" | "monochrome") {
            return Err(format!("{k}: unknown value"));
        }
    }
    let temp = num("temp")?;
    let tint = num("tint")?;
    let exposure = num("exposure")?;
    let contrast = num("contrast")?;
    let highlights = num("highlights")?;
    let shadows = num("shadows")?;
    let clarity = num("clarity")?;
    let vibrance = num("vibrance")?;
    let saturation = num("saturation")?;
    let (whites, blacks) = if kind == Kind::Sigma { (None, None) } else { (num("whites")?, num("blacks")?) };
    if kind == Kind::Sigma && (obj.contains_key("whites") || obj.contains_key("blacks")) {
        return Err("whites/blacks: not available".into());
    }
    if let Some(m) = obj.get("monochrome") {
        st.monochrome = m.as_bool().ok_or("monochrome: not a boolean")?;
    }
    match kind {
        Kind::Sigma => {
            if let Some(x) = temp {
                st.wb_temp = x as f32;
            }
            if let Some(x) = tint {
                st.wb_tint = x as f32;
            }
            if let Some(x) = exposure {
                st.exposure = x;
            }
            if let Some(x) = contrast {
                st.contrast = x;
            }
            if let Some(x) = highlights {
                st.highlight = x;
            }
            if let Some(x) = shadows {
                st.shadow = x;
            }
            if let Some(x) = clarity {
                st.clarity = x as f32;
            }
            if let Some(x) = vibrance {
                st.vibrance = x as f32;
            }
            if let Some(x) = saturation {
                st.saturation = x as f32;
            }
        }
        Kind::Raw | Kind::Finished => {
            let pct = |x: f64| x / 100.0;
            if kind == Kind::Raw {
                if let Some(mode) = obj.get("wb_mode") {
                    let mode = mode.as_str().ok_or("wb_mode: not a text")?;
                    if !RAW_WB_MODES.contains(&mode) {
                        return Err("wb_mode: unknown".into());
                    }
                    st.raw.wb_mode = mode.to_string();
                }
            } else if obj.contains_key("wb_mode") {
                return Err("wb_mode: not available".into());
            }
            if temp.is_some() || tint.is_some() {
                let (k0, t0) = current_wb.unwrap_or((st.raw.temperature, st.raw.tint));
                let (k, t) = if kind == Kind::Finished {
                    let (rt0, rg0) = rawdev::wb::relative_from(k0, t0);
                    rawdev::wb::relative_to(temp.map_or(rt0, |x| x as f32), tint.map_or(rg0, |x| x as f32))
                } else {
                    (temp.map_or(k0, |x| x as f32), tint.map_or(t0, |x| x as f32))
                };
                // only a real change makes the white balance custom
                if (k - k0).abs() > 0.5 || (t - t0).abs() > 0.05 {
                    st.raw.wb_mode = "custom".into();
                    st.raw.temperature = k;
                    st.raw.tint = t;
                }
            }
            if let Some(x) = exposure {
                st.exposure = x;
            }
            if let Some(x) = contrast {
                st.contrast = pct(x);
            }
            if let Some(x) = highlights {
                st.highlight = pct(x);
            }
            if let Some(x) = shadows {
                st.shadow = pct(x);
            }
            if let Some(x) = whites {
                st.raw.whites = pct(x) as f32;
            }
            if let Some(x) = blacks {
                st.raw.blacks = pct(x) as f32;
            }
            if let Some(x) = clarity {
                st.clarity = pct(x) as f32;
            }
            if let Some(x) = vibrance {
                st.vibrance = pct(x) as f32;
            }
            if let Some(x) = saturation {
                st.saturation = pct(x) as f32;
            }
        }
    }
    Ok(())
}

/// Names of the settings fields a change touched (for the history step, same keys as the app).
pub fn changed_fields(before: &Settings, after: &Settings) -> Vec<&'static str> {
    let mut f = Vec::new();
    let mut add = |c: bool, k: &'static str| {
        if c {
            f.push(k);
        }
    };
    add(before.exposure != after.exposure, "exposure");
    add(before.contrast != after.contrast, "contrast");
    add(before.highlight != after.highlight, "highlight");
    add(before.shadow != after.shadow, "shadow");
    add(before.clarity != after.clarity, "clarity");
    add(before.vibrance != after.vibrance, "vibrance");
    add(before.saturation != after.saturation, "saturation");
    add(before.monochrome != after.monochrome, "monochrome");
    add(before.wb_temp != after.wb_temp, "wb_temp");
    add(before.wb_tint != after.wb_tint, "wb_tint");
    add(before.raw.temperature != after.raw.temperature, "raw.temperature");
    add(before.raw.tint != after.raw.tint, "raw.tint");
    add(before.raw.wb_mode != after.raw.wb_mode, "raw.wb_mode");
    add(before.raw.whites != after.raw.whites, "raw.whites");
    add(before.raw.blacks != after.raw.blacks, "raw.blacks");
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_values_round_trip() {
        let mut st = Settings::default();
        apply(Kind::Raw, &mut st, &json!({"exposure": 0.5, "contrast": 20, "whites": -10, "temp": 6000, "tint": 5}), Some((5200.0, 3.0))).unwrap();
        assert_eq!(st.exposure, 0.5);
        assert!((st.contrast - 0.2).abs() < 1e-9);
        assert_eq!(st.raw.wb_mode, "custom");
        assert_eq!(st.raw.temperature, 6000.0);
        let v = values(Kind::Raw, &st, Some((6000.0, 5.0)));
        assert_eq!(v["contrast"], json!(20.0));
        assert_eq!(v["whites"], json!(-10.0));
    }

    #[test]
    fn refuses_unknown_and_out_of_range() {
        let mut st = Settings::default();
        assert!(apply(Kind::Raw, &mut st, &json!({"exposure": 99}), None).is_err());
        assert!(apply(Kind::Raw, &mut st, &json!({"crop": [0, 0, 1, 1]}), None).is_err());
        assert!(apply(Kind::Sigma, &mut st, &json!({"whites": 1}), None).is_err());
        assert!(apply(Kind::Raw, &mut st, &json!({"wb_mode": "../../etc"}), None).is_err());
        assert!(apply(Kind::Raw, &mut st, &json!([1, 2]), None).is_err());
        assert_eq!(st, Settings::default(), "nothing applied on errors before the change");
    }

    #[test]
    fn untouched_white_balance_stays_as_shot() {
        let mut st = Settings::default();
        apply(Kind::Raw, &mut st, &json!({"temp": 5200, "tint": 3, "exposure": 1}), Some((5200.0, 3.0))).unwrap();
        assert_ne!(st.raw.wb_mode, "custom");
    }
}
