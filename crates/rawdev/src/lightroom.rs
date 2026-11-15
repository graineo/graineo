//! Lightroom develop settings (`crs:`) as **starting values** for our sliders.
//!
//! Sources: the settings text of the Lightroom catalog (`Adobe_imageDevelopSettings.text`, a Lua table
//! `s = { Exposure2012 = 0.35, … }`), taken over once into the sidecar (`graineo:LightroomDevelop`,
//! see `library::lr_handover`) – the app never reads the catalog itself – or the `crs:` fields
//! of an XMP sidecar. Both are only read. The mapping is deliberately approximate
//! ("very similar, not pixel-identical"): the reference sliders are not documented, our
//! chain is a different one (the distance is measured against reference exports).

use std::collections::HashMap;

use develop::Settings;

/// A value of the settings.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Number(f64),
    Text(String),
    Bool(bool),
    List(Vec<Value>),
    Table(HashMap<String, Value>),
}

impl Value {
    pub fn number(&self) -> Option<f64> {
        match self {
            Value::Number(v) => Some(*v),
            Value::Text(t) => t.trim().trim_start_matches('+').parse().ok(),
            Value::Bool(b) => Some(*b as u8 as f64),
            _ => None,
        }
    }
    pub fn text(&self) -> Option<&str> {
        match self {
            Value::Text(t) => Some(t),
            _ => None,
        }
    }
    pub fn flag(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            Value::Number(v) => Some(*v != 0.0),
            Value::Text(t) => match t.trim().to_ascii_lowercase().as_str() {
                "true" | "1" => Some(true),
                "false" | "0" => Some(false),
                _ => None,
            },
            _ => None,
        }
    }
}

pub type Values = HashMap<String, Value>;

// ------------------------------------------------------------------------------------------
// Lua table from the catalog

struct Lua<'a> {
    b: &'a [u8],
    i: usize,
    /// current nesting depth (limited so that a crafted file cannot blow the stack)
    depth: usize,
}

/// Deepest allowed nesting of a Lua table (real settings hardly go beyond 6).
const LUA_MAX_DEPTH: usize = 64;

impl Lua<'_> {
    fn empty(&mut self) {
        while self.i < self.b.len() && (self.b[self.i].is_ascii_whitespace() || self.b[self.i] == b',' || self.b[self.i] == b';') {
            self.i += 1;
        }
    }
    fn value(&mut self) -> Option<Value> {
        self.empty();
        let c = *self.b.get(self.i)?;
        match c {
            b'{' => {
                if self.depth >= LUA_MAX_DEPTH {
                    return None;
                }
                self.depth += 1;
                let r = self.table();
                self.depth -= 1;
                r
            }
            b'"' | b'\'' => {
                self.i += 1;
                let mut s = Vec::new();
                while let Some(&ch) = self.b.get(self.i) {
                    self.i += 1;
                    if ch == c {
                        break;
                    }
                    if ch == b'\\' {
                        if let Some(&n) = self.b.get(self.i) {
                            self.i += 1;
                            s.push(match n {
                                b'n' => b'\n',
                                b't' => b'\t',
                                o => o,
                            });
                        }
                        continue;
                    }
                    s.push(ch);
                }
                Some(Value::Text(String::from_utf8_lossy(&s).into_owned()))
            }
            _ => {
                let start = self.i;
                while self.i < self.b.len() && !matches!(self.b[self.i], b',' | b'}' | b'\n' | b';') {
                    self.i += 1;
                }
                let t = std::str::from_utf8(&self.b[start..self.i]).ok()?.trim();
                Some(match t {
                    "true" => Value::Bool(true),
                    "false" => Value::Bool(false),
                    _ => t.parse::<f64>().map(Value::Number).unwrap_or_else(|_| Value::Text(t.to_string())),
                })
            }
        }
    }
    /// Contents of a table from the opening brace.
    fn table(&mut self) -> Option<Value> {
        {
            {
                self.i += 1;
                let mut list = Vec::new();
                let mut tab = HashMap::new();
                loop {
                    self.empty();
                    match self.b.get(self.i)? {
                        b'}' => {
                            self.i += 1;
                            break;
                        }
                        _ => {
                            // key = value or just value
                            let start = self.i;
                            if let Some(k) = self.name() {
                                self.empty();
                                if self.b.get(self.i) == Some(&b'=') {
                                    self.i += 1;
                                    let v = self.value()?;
                                    tab.insert(k, v);
                                    continue;
                                }
                            }
                            self.i = start;
                            if self.b.get(self.i) == Some(&b'[') {
                                // ["key"] = value
                                self.i += 1;
                                let k = self.value()?;
                                self.empty();
                                if self.b.get(self.i) == Some(&b']') {
                                    self.i += 1;
                                }
                                self.empty();
                                if self.b.get(self.i) == Some(&b'=') {
                                    self.i += 1;
                                }
                                let v = self.value()?;
                                if let Some(t) = k.text() {
                                    tab.insert(t.to_string(), v);
                                }
                                continue;
                            }
                            list.push(self.value()?);
                        }
                    }
                }
                Some(if tab.is_empty() { Value::List(list) } else { Value::Table(tab) })
            }
        }
    }
    fn name(&mut self) -> Option<String> {
        let start = self.i;
        while self.i < self.b.len() && (self.b[self.i].is_ascii_alphanumeric() || self.b[self.i] == b'_') {
            self.i += 1;
        }
        (self.i > start && !self.b[start].is_ascii_digit() && self.b[start] != b'-').then(|| String::from_utf8_lossy(&self.b[start..self.i]).into_owned())
    }
}

/// Read the text from `Adobe_imageDevelopSettings.text` ("s = { … }").
pub fn from_catalog(text: &str) -> Option<Values> {
    let start = text.find('{')?;
    let mut l = Lua { b: text[start..].as_bytes(), i: 0, depth: 0 };
    match l.value()? {
        Value::Table(t) => Some(t),
        Value::List(v) if v.is_empty() => Some(HashMap::new()),
        _ => None,
    }
}

// ------------------------------------------------------------------------------------------
// XMP (crs:)

/// `crs:` fields of an XMP file (attributes and elements; curves as a list of numbers x, y, …).
pub fn from_xmp(text: &str) -> Values {
    let mut w = HashMap::new();
    let mut rest = text;
    // attributes crs:Name="value"
    while let Some(i) = rest.find("crs:") {
        let r = &rest[i + 4..];
        let n_end = r.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(r.len());
        let name = &r[..n_end];
        let after = &r[n_end..];
        if let Some(v) = after.strip_prefix("=\"") {
            if let Some(e) = v.find('"') {
                w.insert(name.to_string(), Value::Text(v[..e].replace("&amp;", "&").replace("&quot;", "\"")));
            }
        } else if let Some(after) = after.strip_prefix('>') {
            // element: Seq/Bag with rdf:li or plain text
            if let Some(e) = after.find(&format!("</crs:{name}>")) {
                let content = &after[..e];
                if content.contains("<rdf:li") {
                    let mut numbers = Vec::new();
                    let mut texts = Vec::new();
                    let mut s = content;
                    while let Some(a) = s.find("<rdf:li") {
                        let Some(b) = s[a..].find('>') else { break };
                        let st = a + b + 1;
                        let Some(z) = s[st..].find("</rdf:li>") else { break };
                        let t = s[st..st + z].trim();
                        texts.push(t.to_string());
                        for part in t.split(',') {
                            if let Ok(v) = part.trim().parse::<f64>() {
                                numbers.push(Value::Number(v));
                            }
                        }
                        s = &s[st + z..];
                    }
                    if !numbers.is_empty() {
                        w.insert(name.to_string(), Value::List(numbers));
                    } else if let Some(t) = texts.first() {
                        w.insert(name.to_string(), Value::Text(t.clone()));
                    }
                } else if !content.contains('<') {
                    w.insert(name.to_string(), Value::Text(content.trim().to_string()));
                }
            }
        }
        rest = &rest[i + 4..];
    }
    // The simple XMP reader does not understand nested Lightroom masks.
    // Report such data explicitly instead of treating it as an empty mask list.
    for key in ["MaskGroupBasedCorrections", "GradientBasedCorrections", "CircularGradientBasedCorrections", "PaintBasedCorrections"] {
        if let Some(i) = text.find(&format!("<crs:{key}")) {
            let closing = format!("</crs:{key}>");
            let block = text[i..].split(&closing).next().unwrap_or("");
            if block.contains("<rdf:li") {
                w.insert(key.into(), Value::Text("Unsupported nested XMP mask".into()));
            }
        }
    }
    w
}

// ------------------------------------------------------------------------------------------
// Mapping

/// What the mapping needs to know about the image.
pub struct ImageContext<'a> {
    /// short camera name for look names (`x100v`, `eos-rp` …)
    pub camera_short: &'a str,
    /// bundled looks of this camera (`camera/style`)
    pub looks: &'a [String],
    /// camera orientation in quarter turns (Lightroom's crop refers to the upright image)
    pub orientation: u8,
    /// finished image (JPEG …): Lightroom's temperature/tint are relative then (−100 … +100)
    pub finished: bool,
    /// sensor orientation, before crop and quarter turn (mask coordinates).
    pub size: [usize; 2],
}

/// Lightroom's profile name → our look (`""` = automatic, `"neutral"`, `camera/style`).
pub fn look_from_profile(profile: &str, b: &ImageContext) -> (String, bool) {
    let p = profile.trim().to_ascii_lowercase();
    let mono = p.contains("monochrom") || p.contains("acros") || p.contains("b&w") || p.contains("schwarzweiß");
    if !p.starts_with("camera ") {
        // Adobe Standard/Color, Embedded, Adobe Monochrome …: our neutral development
        return ("neutral".into(), p.contains("adobe monochrome"));
    }
    let n = p.trim_start_matches("camera ").trim();
    let keys: &[(&str, &str)] = &[
        ("classic chrome", "classic-chrome"),
        ("classic neg", "classic-neg"),
        ("pro neg. std", "pro-neg-std"),
        ("pro neg std", "pro-neg-std"),
        ("pro neg. hi", "pro-neg-hi"),
        ("provia", "provia"),
        ("velvia", "velvia"),
        ("astia", "astia"),
        ("eterna", "eterna"),
        ("acros+ye", "acros-ye"),
        ("acros+r", "acros-r"),
        ("acros+g", "acros-g"),
        ("acros", "acros"),
        ("monochrome+ye", "monochrome-ye"),
        ("monochrome+r", "monochrome-r"),
        ("monochrome+g", "monochrome-g"),
        ("monochrome", "monochrome"),
        ("portrait", "portrait"),
        ("neutral", "neutral"),
        ("faithful", "faithful"),
        ("landscape", "landscape"),
        ("vivid", "vivid"),
        ("standard", "standard"),
    ];
    for (lr, ours) in keys {
        if n.contains(lr) {
            let name = format!("{}/{ours}", b.camera_short);
            if b.looks.contains(&name) {
                return (name, mono);
            }
            // same style with a different suffix (e.g. user1-standard)
            if let Some(l) = b.looks.iter().find(|l| l.ends_with(&format!("-{ours}")) || l.ends_with(&format!("/{ours}"))) {
                return (l.clone(), mono);
            }
            break;
        }
    }
    ("neutral".into(), mono)
}

fn curve(w: Option<&Value>) -> Option<Vec<[f32; 2]>> {
    let Value::List(l) = w? else { return None };
    let z: Vec<f64> = l.iter().filter_map(Value::number).collect();
    if z.len() < 4 || z.len() % 2 != 0 {
        return None;
    }
    let mut p: Vec<[f32; 2]> = z.chunks(2).map(|c| [(c[0] / 255.0) as f32, (c[1] / 255.0) as f32]).collect();
    p.sort_by(|a, b| a[0].total_cmp(&b[0]));
    // diagonal = no curve
    if p.iter().all(|q| (q[0] - q[1]).abs() < 1e-3) && p.first().is_some_and(|q| q[0] < 1e-3) && p.last().is_some_and(|q| q[0] > 0.999) {
        return Some(Vec::new());
    }
    Some(p)
}

/// The sliders of a look (film profile: highlights, shadows, saturation, color mixer, tone curve …) act in
/// Lightroom as a separate layer on top of the image's sliders, with the intensity `Look.Amount`.
/// Here they are added to the image's values (curves chained), so that the rest of the import
/// continues unchanged. Sharpening and table references are left out.
fn with_look(w: &Values) -> Values {
    let Some(Value::Table(l)) = w.get("Look") else { return w.clone() };
    let Some(Value::Table(p)) = l.get("Parameters") else { return w.clone() };
    let a = l.get("Amount").and_then(Value::number).unwrap_or(1.0).clamp(0.0, 2.0);
    let mut o = w.clone();
    let has_parametric = ["ParametricShadows", "ParametricDarks", "ParametricLights", "ParametricHighlights"].iter().any(|k| p.get(*k).and_then(Value::number).is_some_and(|v| v != 0.0));
    for (k, v) in p {
        match k.as_str() {
            "ProcessVersion" | "Version" | "RGBTable" | "UUID" | "ToneCurveName2012" | "Sharpness" | "SharpenRadius" | "SharpenDetail" | "SharpenEdgeMasking" | "LuminanceSmoothing" | "LuminanceNoiseReductionDetail"
            | "LuminanceNoiseReductionContrast" | "ColorNoiseReduction" | "ColorNoiseReductionDetail" | "ColorNoiseReductionSmoothness" | "Amount" => {}
            "ParametricShadowSplit" | "ParametricMidtoneSplit" | "ParametricHighlightSplit" => {
                if has_parametric {
                    if let Some(x) = v.number() {
                        o.insert(k.clone(), Value::Number(x));
                    }
                }
            }
            "ToneCurvePV2012" | "ToneCurvePV2012Red" | "ToneCurvePV2012Green" | "ToneCurvePV2012Blue" => {
                let Some(pl) = curve(Some(v)).filter(|c| !c.is_empty()) else { continue };
                let user_curve = curve(w.get(k)).filter(|c| !c.is_empty());
                // first the look's curve, then the image's
                let points: Vec<Value> = (0..=16)
                    .flat_map(|i| {
                        let x = i as f32 / 16.0;
                        let mut y = develop::curve::eval(&pl, x);
                        if a != 1.0 {
                            y = x + a as f32 * (y - x);
                        }
                        if let Some(n) = &user_curve {
                            y = develop::curve::eval(n, y);
                        }
                        [Value::Number((x * 255.0) as f64), Value::Number((y.clamp(0.0, 1.0) * 255.0) as f64)]
                    })
                    .collect();
                o.insert(k.clone(), Value::List(points));
            }
            "ConvertToGrayscale" => {
                if v.flag() == Some(true) {
                    o.insert(k.clone(), Value::Bool(true));
                }
            }
            _ => {
                if let (Some(x), true) = (v.number(), !matches!(v, Value::Bool(_) | Value::Text(_))) {
                    if x != 0.0 {
                        let own = w.get(k).and_then(Value::number).unwrap_or(0.0);
                        o.insert(k.clone(), Value::Number(own + a * x));
                    }
                }
            }
        }
    }
    o
}

/// Lightroom settings → our settings (starting values). `base`: starting point (e.g. default).
/// Tuning of the Lightroom sliders (process 2012) to our development: factor and exponent for highlights and shadows, factor for contrast and clarity.
const LR_HIGHLIGHTS: (f64, f64) = (0.27, 0.39);
const LR_SHADOWS: (f64, f64) = (0.75, 0.85);
const LR_CONTRAST: f64 = 1.33;
const LR_CLARITY: f64 = 0.125;
/// Lightroom Dehaze → `Settings::dehaze`. Checked against the only two reference exports with Dehaze in the user's catalog
/// (+33, +49): ΔE00 mean 7.2 without, 6.24 at 0.8, 6.19 at 1.0, 6.29 at 1.2; mid tones closest at 1.0. Weak evidence (two photos).
const LR_DEHAZE: f64 = 1.0;
/// Tuning of the old process version (2003/2010): exposure offset, contrast factor, black offset, fill light factor.
const PV10_EXPOSURE: f64 = 0.07;
const PV10_CONTRAST: f64 = 2.0;
const PV10_BLACKS: f64 = -0.1;
const PV10_FILL_LIGHT: f64 = 0.75;
/// Positive exposure of the old process version: share that we apply, and added contrast per EV.
const PV10_EXPOSURE_GAIN: f64 = 0.8;
const PV10_CONTRAST_PER_EV: f64 = 0.3;

fn tuned(v: f64, factor: f64, exponent: f64) -> f64 {
    v.signum() * factor * v.abs().powf(exponent)
}

pub fn to_settings(w: &Values, b: &ImageContext, base: &Settings) -> Settings {
    let with_look_values;
    let w = if w.contains_key("Look") && !std::env::var("GRAINEO_LR_LOOK_SLIDERS_OFF").is_ok() {
        with_look_values = with_look(w);
        &with_look_values
    } else {
        w
    };
    let mut s = base.clone();
    // only finite numbers: `inf`/`nan` from a damaged catalog must not reach crop or sliders
    let z = |k: &str| w.get(k).and_then(Value::number).filter(|v| v.is_finite());
    // Panel switches ("Enable…", missing = on). Cross-check on 19 exports: ignoring them
    // worsens the mean from 9.0 to 9.5 ΔE00.
    let ignored = std::env::var("GRAINEO_LR_IGNORE").unwrap_or_default();
    let on = |k: &str| ignored.split(',').any(|x| x == k) || w.get(k).and_then(Value::flag).unwrap_or(true);
    let pv: f64 = w.get("ProcessVersion").and_then(|v| v.text().and_then(|t| t.parse().ok()).or(v.number())).unwrap_or(15.0);
    let is_pv2012 = pv >= 6.6 || z("Exposure2012").is_some_and(|v| v != 0.0) || z("Contrast2012").is_some_and(|v| v != 0.0);
    s.raw.from_lightroom = true;
    // basic settings
    if is_pv2012 {
        // Our sliders act differently from Lightroom's at the same numeric value (highlights and shadows much stronger, clarity much stronger,
        // contrast weaker). Factors and exponents are tuned on 600 reference exports (ΔE00, check on other photos:
        // moderately edited 8.8 → 6.7, heavily 8.0 → 6.1; unedited unchanged).
        s.exposure = z("Exposure2012").unwrap_or(0.0);
        s.contrast = z("Contrast2012").unwrap_or(0.0) / 100.0 * LR_CONTRAST;
        s.highlight = tuned(z("Highlights2012").unwrap_or(0.0) / 100.0, LR_HIGHLIGHTS.0, LR_HIGHLIGHTS.1);
        s.shadow = tuned(z("Shadows2012").unwrap_or(0.0) / 100.0, LR_SHADOWS.0, LR_SHADOWS.1);
        s.raw.whites = (z("Whites2012").unwrap_or(0.0) / 100.0) as f32;
        s.raw.blacks = (z("Blacks2012").unwrap_or(0.0) / 100.0) as f32;
        s.clarity = (z("Clarity2012").unwrap_or(0.0) / 100.0 * LR_CLARITY) as f32;
    } else {
        // process 2003/2010: exposure, brightness (50), contrast (25), fill light, recovery, blacks (5)
        // Tuned on 231 reference exports (process 2003/2010, mostly Canon 400D/30D/5D II), checked on 161 other photos:
        // color difference (ΔE00, median) 6.1 → 5.4. In the reference the contrast acts about twice as strongly as ours, the fill light
        // about three quarters. Reading the built-in "Medium Contrast" curve worsened the match (6.5 vs. 5.4) and
        // is left out.
        s.exposure = PV10_EXPOSURE + z("Exposure").unwrap_or(0.0) + (z("Brightness").unwrap_or(50.0) - 50.0) / 100.0;
        s.contrast = (z("Contrast").unwrap_or(25.0) - 25.0) / 100.0 * PV10_CONTRAST;
        s.shadow = z("FillLight").unwrap_or(0.0) / 100.0 * PV10_FILL_LIGHT;
        s.highlight = -z("HighlightRecovery").unwrap_or(0.0) / 100.0;
        s.raw.blacks = (-(z("Shadows").unwrap_or(5.0) - 5.0) / 50.0 + PV10_BLACKS).clamp(-1.0, 1.0) as f32;
        s.clarity = (z("Clarity").unwrap_or(0.0) / 100.0) as f32;
        // Large positive exposure (old photos pushed by +1 … +3.7 EV): the reference brightens the mid tones less and keeps
        // the shadows deeper than our exposure does. Tuned on 90 reference exports (60 with exposure ≥ 1 EV): ΔE00 median
        // 6.8 → 5.0, those with ≥ 1 EV 7.6 → 5.8, the others 4.2 → 3.9.
        let push = z("Exposure").unwrap_or(0.0).max(0.0);
        s.exposure -= (1.0 - PV10_EXPOSURE_GAIN) * push;
        s.contrast += PV10_CONTRAST_PER_EV * push;
    }
    s.raw.texture = (z("Texture").unwrap_or(0.0) / 100.0) as f32;
    s.vibrance = (z("Vibrance").unwrap_or(0.0) / 100.0) as f32;
    // Lightroom "Dunst entfernen" (−100 … +100) → our −1 … +1; factor 0.8 by eye (not yet measured against Lightroom)
    s.dehaze = (z("Dehaze").unwrap_or(0.0) / 100.0 * LR_DEHAZE).clamp(-1.0, 1.0) as f32;
    s.saturation = (z("Saturation").unwrap_or(0.0) / 100.0) as f32;
    // white balance
    // without information: with temperature "Custom", otherwise as shot
    // Finished images (JPEG, HEIC, TIFF) have relative sliders in Lightroom: IncrementalTemperature/IncrementalTint (−100 … +100)
    let (temp, tint) = if b.finished { (z("IncrementalTemperature").or(z("Temperature")), z("IncrementalTint").or(z("Tint"))) } else { (z("Temperature"), z("Tint")) };
    let wb = w.get("WhiteBalance").and_then(Value::text).unwrap_or(if temp.is_some() { "Custom" } else { "As Shot" });
    match (wb, temp, tint) {
        ("As Shot", _, _) => s.raw.wb_mode = "as_shot".into(),
        (_, Some(t), tint) if b.finished => {
            let (k, g) = crate::wb::relative_to(t as f32, tint.unwrap_or(0.0) as f32);
            s.raw.wb_mode = if t == 0.0 && tint.unwrap_or(0.0) == 0.0 { "as_shot".into() } else { "custom".into() };
            s.raw.temperature = k;
            s.raw.tint = g;
        }
        (_, Some(t), tint) => {
            s.raw.wb_mode = "custom".into();
            s.raw.temperature = t as f32;
            s.raw.tint = tint.unwrap_or(0.0) as f32;
        }
        ("Auto", None, _) => s.raw.wb_mode = "auto".into(),
        (v, None, _) => {
            s.raw.wb_mode = match v {
                "Daylight" => "daylight",
                "Cloudy" => "cloudy",
                "Shade" => "shade",
                "Tungsten" => "tungsten",
                "Fluorescent" => "fluorescent",
                "Flash" => "flash",
                _ => "as_shot",
            }
            .into()
        }
    }
    // tone curve
    if on("EnableToneCurve") {
        let mut tc = develop::ToneCurve::default();
        // process 2012: `ToneCurvePV2012…`; the curves of the old process version (default "Medium Contrast") are not read (see above)
        let curves: &[(&str, usize)] = if is_pv2012 { &[("ToneCurvePV2012", 0), ("ToneCurvePV2012Red", 1), ("ToneCurvePV2012Green", 2), ("ToneCurvePV2012Blue", 3)] } else { &[] };
        for &(k, ch) in curves {
            if let Some(p) = curve(w.get(k)) {
                *tc.channel_mut(ch) = p;
            }
        }
        // parametric curve (highlights, lights, darks, shadows with three
        // split points) placed as a point curve before the point curve
        let par = [z("ParametricShadows"), z("ParametricDarks"), z("ParametricLights"), z("ParametricHighlights")].map(|v| v.unwrap_or(0.0) as f32);
        if par.iter().any(|v| *v != 0.0) {
            let sp = [z("ParametricShadowSplit").unwrap_or(25.0), z("ParametricMidtoneSplit").unwrap_or(50.0), z("ParametricHighlightSplit").unwrap_or(75.0)].map(|v| (v / 100.0) as f32);
            let bounds = [0.0, sp[0], sp[1], sp[2], 1.0];
            let mut points = vec![[0.0f32, 0.0f32]];
            for i in 0..4 {
                let x = 0.5 * (bounds[i] + bounds[i + 1]);
                // ±100 shifts the center of the range by up to ≈ 0.1 (roughly measured)
                let y = (x + par[i] / 100.0 * 0.1).clamp(0.0, 1.0);
                points.push([x, y]);
            }
            points.push([1.0, 1.0]);
            let rgb = tc.rgb.clone();
            tc.rgb = (0..=16)
                .map(|k| {
                    let x = k as f32 / 16.0;
                    let y = develop::curve::eval(&points, x);
                    let y = if rgb.is_empty() { y } else { develop::curve::eval(&rgb, y) };
                    [x, y]
                })
                .collect();
        }
        s.tone_curve = tc;
    }
    // color mixer
    if on("EnableColorAdjustments") {
        const COLORS: [&str; 8] = ["Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta"];
        for (i, f) in COLORS.iter().enumerate() {
            s.hsl[i] = [
                (z(&format!("HueAdjustment{f}")).unwrap_or(0.0) / 100.0) as f32,
                (z(&format!("SaturationAdjustment{f}")).unwrap_or(0.0) / 100.0) as f32,
                (z(&format!("LuminanceAdjustment{f}")).unwrap_or(0.0) / 100.0) as f32,
            ];
        }
    }
    // black & white
    s.monochrome = w.get("ConvertToGrayscale").and_then(Value::flag).unwrap_or(false);
    // black & white mixer: applies if the photo is converted to black & white
    for (i, f) in ["Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta"].iter().enumerate() {
        s.gray_mix[i] = (z(&format!("GrayMixer{f}")).unwrap_or(0.0) / 100.0).clamp(-1.0, 1.0) as f32;
    }
    // Upright: only the mode (straightening happens on open with our own detection)
    s.raw.upright_from_lr = z("PerspectiveUpright").map(|v| v as u8).filter(|v| (1..=4).contains(v));
    // calibration: primaries of the profile and shadow tint
    if on("EnableCalibration") {
        for (i, k) in ["RedHue", "RedSaturation", "GreenHue", "GreenSaturation", "BlueHue", "BlueSaturation", "ShadowTint"].iter().enumerate() {
            s.raw.calibration[i] = (z(k).unwrap_or(0.0) / 100.0).clamp(-1.0, 1.0) as f32;
        }
    }
    // color grading or split toning
    if on("EnableSplitToning") {
        let sat = |k: &str| (z(k).unwrap_or(0.0) / 100.0) as f32;
        let hue = |k: &str, d: f32| z(k).map(|v| v as f32).unwrap_or(d);
        if w.contains_key("ColorGradeShadowSat") || w.contains_key("ColorGradeMidtoneSat") || w.contains_key("ColorGradeHighlightSat") {
            s.grade_shadows = [hue("ColorGradeShadowHue", 220.0), sat("ColorGradeShadowSat")];
            s.grade_midtones = [hue("ColorGradeMidtoneHue", 40.0), sat("ColorGradeMidtoneSat")];
            s.grade_highlights = [hue("ColorGradeHighlightHue", 45.0), sat("ColorGradeHighlightSat")];
        }
        if sat("ColorGradeShadowSat") == 0.0 && sat("ColorGradeHighlightSat") == 0.0 {
            if let (Some(_), Some(_)) = (z("SplitToningShadowSaturation"), z("SplitToningHighlightSaturation")) {
                s.grade_shadows = [hue("SplitToningShadowHue", 220.0), sat("SplitToningShadowSaturation")];
                s.grade_highlights = [hue("SplitToningHighlightHue", 45.0), sat("SplitToningHighlightSaturation")];
            }
        }
        s.grade_balance = (z("SplitToningBalance").or(z("ColorGradeBalance")).unwrap_or(0.0) / 100.0) as f32;
    }
    // details: sharpening and noise reduction
    if on("EnableDetail") {
        let sh = z("Sharpness").unwrap_or(40.0);
        s.sharpen = (((sh - 40.0) / 100.0).max(0.0) * 1.5) as f32;
        s.sharpen_radius = z("SharpenRadius").unwrap_or(1.0).clamp(0.5, 3.0) as f32;
        s.sharpen_mask = (z("SharpenEdgeMasking").unwrap_or(0.0) / 100.0) as f32;
        s.raw.nr_luma = z("LuminanceSmoothing").unwrap_or(0.0) as f32;
        s.raw.nr_luma_detail = z("LuminanceNoiseReductionDetail").unwrap_or(50.0) as f32;
        s.raw.nr_color = z("ColorNoiseReduction").unwrap_or(25.0) as f32;
        s.raw.nr_color_detail = z("ColorNoiseReductionDetail").unwrap_or(50.0) as f32;
    }
    // lens correction
    if on("EnableLensCorrections") {
        s.raw.lens = z("LensProfileEnable").unwrap_or(0.0) != 0.0;
        s.raw.lens_distortion = (z("LensProfileDistortionScale").unwrap_or(100.0) / 100.0) as f32;
        s.raw.lens_vignette = (z("LensProfileVignettingScale").unwrap_or(100.0) / 100.0) as f32;
        s.raw.lens_ca = z("AutoLateralCA").unwrap_or(1.0) != 0.0 || s.raw.lens;
        s.defringe_purple_amount = (z("DefringePurpleAmount").unwrap_or(0.0) / 20.0) as f32;
        s.defringe_purple_hue = [z("DefringePurpleHueLo").unwrap_or(30.0) as f32, z("DefringePurpleHueHi").unwrap_or(70.0) as f32];
        s.defringe_green_amount = (z("DefringeGreenAmount").unwrap_or(0.0) / 20.0) as f32;
        s.defringe_green_hue = [z("DefringeGreenHueLo").unwrap_or(40.0) as f32, z("DefringeGreenHueHi").unwrap_or(60.0) as f32];
    } else {
        s.raw.lens = false;
    }
    // effects
    if on("EnableEffects") {
        s.vignette = (z("PostCropVignetteAmount").unwrap_or(0.0) / 100.0) as f32;
        s.vignette_mid = (z("PostCropVignetteMidpoint").unwrap_or(50.0) / 100.0) as f32;
        s.grain_amount = (z("GrainAmount").unwrap_or(0.0) / 100.0) as f32;
        s.grain_size = (z("GrainSize").unwrap_or(25.0) / 25.0).clamp(0.5, 3.0) as f32;
        s.grain_rough = (z("GrainFrequency").unwrap_or(50.0) / 100.0) as f32;
    }
    // profile
    if let Some(p) = w.get("CameraProfile").and_then(Value::text) {
        let (look, mono) = look_from_profile(p, b);
        s.raw.look = look;
        s.monochrome |= mono;
    } else {
        s.raw.look = "neutral".into();
    }
    // look (film profile, e.g. RNI Films): matched to the installed profile via its identifier
    if let Some(Value::Table(l)) = w.get("Look").filter(|_| std::env::var_os("GRAINEO_LR_LOOK_OFF").is_none()) {
        let adobe = l.get("Name").and_then(Value::text).is_some_and(|n| n.to_ascii_lowercase().starts_with("adobe "))
            || l.get("Group").and_then(Value::text).is_some_and(|n| n.to_ascii_lowercase().starts_with("adobe"));
        if let Some(u) = l.get("UUID").filter(|_| !adobe).and_then(Value::text).filter(|u| !u.is_empty() && u.len() <= 128 && u.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')) {
            s.raw.look = format!("film:{}", u.to_uppercase());
            s.raw.look_name = l.get("Name").and_then(Value::text).unwrap_or(u).to_string();
            s.raw.look_package = l.get("Group").and_then(Value::text).unwrap_or("").to_string();
            crate::film_profiles::remember_reference(&mut s);
            if let Some(a) = l.get("Amount").and_then(Value::number) {
                s.profile_amount = a.clamp(0.0, 1.0) as f32;
            }
        }
    }
    // crop (relative to the upright image; Lightroom's values refer to the
    // image in camera orientation, see [`crop`])
    // `HasCrop` is missing in some catalog entries although Lightroom crops: then the values themselves count
    let crop_active = match w.get("HasCrop").and_then(Value::flag) {
        Some(b) => b,
        None => {
            z("CropLeft").is_some_and(|v| v > 1e-4)
                || z("CropTop").is_some_and(|v| v > 1e-4)
                || z("CropRight").is_some_and(|v| v < 1.0 - 1e-4)
                || z("CropBottom").is_some_and(|v| v < 1.0 - 1e-4)
                || z("CropAngle").is_some_and(|v| v.abs() > 1e-4)
        }
    };
    // A crop without area (e.g. only `CropRight = 0, CropBottom = 0` without the other edges, as Lightroom stores it for
    // reset images) is no crop: otherwise a single-colored pixel would remain instead of the image.
    let edges = (z("CropLeft").unwrap_or(0.0), z("CropTop").unwrap_or(0.0), z("CropRight").unwrap_or(1.0), z("CropBottom").unwrap_or(1.0));
    let crop_active = crop_active && (edges.2 - edges.0).abs() >= 0.02 && (edges.3 - edges.1).abs() >= 0.02;
    if crop_active {
        let (l, t, r, bt) = edges;
        let (crop, angle) = crop([l as f32, t as f32, r as f32, bt as f32], z("CropAngle").unwrap_or(0.0) as f32, b.orientation);
        s.crop = crop;
        s.crop_angle = angle;
    }
    // Own local edits are the truth and are not overwritten.
    if base.local.is_none() {
        let (local, report) = crate::lr_masks::import(w, b.size);
        s.local = local;
        s.raw.lightroom_masks = report;
    }
    // retouching and red eye: as with masks, what was created in graineo itself wins
    if base.spots.is_empty() && base.red_eyes.is_empty() {
        let (spots_found, red_eyes_found) = retouch_from_lightroom(w);
        s.spots = spots_found;
        s.red_eyes = red_eyes_found;
    }
    s
}

/// Lightroom's spot removal (`RetouchInfo`: center, radius, source, "heal" or "clone") and red eye (`RedEyeInfo`).
/// As with masks, coordinates refer to the unrotated, uncropped image, radii to the image width
/// (confirmed on a photo with a visible spot). The size of the red pupil is an assumption (semi-axes in image widths).
pub fn retouch_from_lightroom(w: &Values) -> (Vec<develop::spots::Spot>, Vec<develop::spots::RedEye>) {
    let number = |t: &Values, k: &str| t.get(k).and_then(Value::number);
    let mut spots_found = Vec::new();
    if let Some(Value::List(l)) = w.get("RetouchInfo") {
        for e in l {
            let Value::Table(t) = e else { continue };
            let (Some(cx), Some(cy), Some(r), Some(sx), Some(sy)) = (number(t, "centerX"), number(t, "centerY"), number(t, "radius"), number(t, "sourceX"), number(t, "sourceY")) else { continue };
            let is_clone = t.get("spotType").and_then(Value::text).is_some_and(|v| v.eq_ignore_ascii_case("clone"));
            spots_found.push(develop::spots::Spot { clone: is_clone, center: [cx as f32, cy as f32], source: [sx as f32, sy as f32], radius: r as f32, opacity: number(t, "opacity").unwrap_or(1.0).clamp(0.0, 1.0) as f32 });
        }
    }
    let mut red_eyes_found = Vec::new();
    if let Some(Value::List(l)) = w.get("RedEyeInfo") {
        for e in l {
            let Value::Table(t) = e else { continue };
            let Some(Value::Table(p)) = t.get("pupil") else { continue };
            let Some(Value::Table(el)) = p.get("ellipse") else { continue };
            let (Some(cx), Some(cy), Some(sx), Some(sy)) = (number(el, "centerX"), number(el, "centerY"), number(el, "sizeX"), number(el, "sizeY")) else { continue };
            red_eyes_found.push(develop::spots::RedEye {
                center: [cx as f32, cy as f32],
                size: [sx as f32, sy as f32],
                angle: number(el, "alpha").unwrap_or(0.0) as f32,
                density: number(p, "density").unwrap_or(0.8).clamp(0.0, 1.0) as f32,
                darken: number(t, "pupilDarkenAmount").unwrap_or(0.5).clamp(0.0, 1.0) as f32,
            });
        }
    }
    (spots_found, red_eyes_found)
}

/// Lightroom's crop (left, top, right, bottom relative to the image in sensor orientation) → our
/// frame relative to the upright image (center x/y, width, height) and angle.
pub fn crop(r: [f32; 4], angle: f32, q: u8) -> ([f32; 4], f32) {
    let [l, t, rr, b] = r;
    // rectangle in sensor orientation → upright (quarter turns clockwise)
    let (l2, t2, r2, b2) = match q % 4 {
        1 => (1.0 - b, l, 1.0 - t, rr),
        2 => (1.0 - rr, 1.0 - b, 1.0 - l, 1.0 - t),
        3 => (t, 1.0 - rr, b, 1.0 - l),
        _ => (l, t, rr, b),
    };
    ([(l2 + r2) / 2.0, (t2 + b2) / 2.0, (r2 - l2).abs(), (b2 - t2).abs()], angle)
}

fn start_values_on() -> bool {
    library::settings::value("lightroomStartValues").as_bool() != Some(false)
}

/// Lightroom data in the sidecar of the image (taken over once from the catalog, [`library::lr_handover`]).
/// Photo of the library: from the library (the main store, [`library::store`]); otherwise from the sidecar next to it.
fn sidecar_lightroom(doc: &crate::RawDoc) -> library::xmp::Lightroom {
    library::store::lightroom_for(&doc.logical_path, None)
}

fn set_orientation(doc: &mut crate::RawDoc, code: Option<&str>) {
    if let Some(exif) = code.and_then(library::db::orientation_from_lightroom) {
        doc.orientation = crate::doc::quarters(exif as u16);
    }
}

/// Adopt the orientation of the image in Lightroom (including a rotation set there by hand): the document
/// is then oriented that way, and the crop is converted to match. Read from the sidecar
/// (`graineo:LightroomOrientation`, taken over from the catalog), never from the catalog itself; only
/// if the Lightroom starting values are not disabled (`lightroomStartValues`).
pub fn apply_orientation(doc: &mut crate::RawDoc) {
    if !start_values_on() {
        return;
    }
    let lr = sidecar_lightroom(doc);
    // as before: the orientation counts only together with Lightroom settings
    if lr.develop.is_some() {
        set_orientation(doc, lr.orientation.as_deref());
    }
}

/// Like [`apply_orientation`], straight from a catalog (read only) – for measuring tools and to check the handover.
pub fn apply_orientation_from_catalog(doc: &mut crate::RawDoc, catalog: &std::path::Path) {
    if !start_values_on() {
        return;
    }
    let Ok(e) = library::lr_develop::read(catalog, &doc.logical_path) else { return };
    let code = e.into_iter().find(|e| e.copy.is_none()).and_then(|e| e.orientation);
    set_orientation(doc, code.as_deref());
}

/// Is the file itself a reference export ("Lightroom" as creator in the file header)? Then the sliders are already applied.
pub fn is_lightroom_export(path_: &std::path::Path) -> bool {
    use std::io::Read;
    let mut head = vec![0u8; 256 * 1024];
    let n = std::fs::File::open(path_).and_then(|mut f| f.read(&mut head)).unwrap_or(0);
    let t = String::from_utf8_lossy(&head[..n]);
    t.match_indices("CreatorTool").any(|(i, _)| t[i..(i + 120).min(t.len())].contains("Lightroom"))
        || t.contains("Adobe Photoshop Lightroom")
}

/// Starting values for an opened image: the Lightroom settings taken over into the sidecar
/// (`graineo:LightroomDevelop`, can be disabled with the setting `lightroomStartValues`), otherwise the `crs:`
/// fields of the XMP next to the image. The Lightroom catalog itself is never read.
pub fn initial_settings(doc: &crate::RawDoc) -> Option<Settings> {
    start_settings(doc, || sidecar_lightroom(doc).develop)
}

/// Starting values of a virtual copy from its own Lightroom settings (in the copy's entry of the sidecar); `None` if it has
/// none (then those of the original apply, [`initial_settings`]).
pub fn copy_settings(doc: &crate::RawDoc, copy: &str) -> Option<Settings> {
    // library or sidecar ([`library::store::lightroom_for`]; the copies belong to the photo the data was taken over for)
    let own = || -> Option<String> { library::store::lightroom_for(&doc.logical_path, Some(copy)).develop };
    if !start_values_on() {
        return None;
    }
    let text = own()?;
    start_settings(doc, || Some(text))
}

/// Starting values from a given Lightroom settings text (as [`initial_settings`] interprets it) – for checking tools.
pub fn settings_from_text(doc: &crate::RawDoc, text: &str) -> Option<Settings> {
    start_settings(doc, || Some(text.to_string()))
}

/// Like [`initial_settings`], straight from a catalog (read only) – for measuring tools and to check the handover.
pub fn initial_settings_from_catalog(doc: &crate::RawDoc, catalog: &std::path::Path) -> Option<Settings> {
    start_settings(doc, || library::lr_develop::read(catalog, &doc.logical_path).ok()?.into_iter().find(|e| e.copy.is_none()).map(|e| e.text))
}

fn start_settings(doc: &crate::RawDoc, lightroom_text: impl FnOnce() -> Option<String>) -> Option<Settings> {
    let short = raw::picture_style::camera_short(&doc.info.maker, &doc.info.model);
    let img = ImageContext { camera_short: &short, looks: &doc.looks, orientation: doc.orientation, finished: doc.finished, size: doc.full_size };
    // finished images: can be disabled (setting "apply Lightroom edits to JPEGs"); a reference export already carries its edits
    if doc.finished && (library::settings::value("lightroomJpegEdits").as_bool() == Some(false) || is_lightroom_export(&doc.path)) {
        return None;
    }
    let lightroom_values = || -> Option<Values> {
        if !start_values_on() {
            return None;
        }
        self::from_catalog(&lightroom_text()?)
    };
    let values = lightroom_values().filter(|v| !v.is_empty()).or_else(|| {
        let t = library::xmp::text(&library::xmp::path_to(&doc.logical_path))?;
        t.contains("crs:").then(|| from_xmp(&t))
    })?;
    if values.is_empty() {
        return None;
    }
    Some(to_settings(&values, &img, &Settings::default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_image() -> ImageContext<'static> {
        ImageContext { camera_short: "gr3", looks: &[], orientation: 0, finished: false, size: [600, 400] }
    }

    /// Finished images: Lightroom stores the white balance relatively (IncrementalTemperature/-Tint).
    #[test]
    fn white_balance_relative_for_finished_images() {
        let t = "s = { IncrementalTemperature = 20,\nIncrementalTint = -10,\nWhiteBalance = \"Custom\" }";
        let b = ImageContext { finished: true, ..empty_image() };
        let s = to_settings(&from_catalog(t).unwrap(), &b, &Settings::default());
        assert_eq!(s.raw.wb_mode, "custom");
        let (k, g) = crate::wb::relative_to(20.0, -10.0);
        assert_eq!((s.raw.temperature, s.raw.tint), (k, g));
        let t = "s = { IncrementalTemperature = 0,\nIncrementalTint = 0,\nWhiteBalance = \"As Shot\" }";
        assert_eq!(to_settings(&from_catalog(t).unwrap(), &b, &Settings::default()).raw.wb_mode, "as_shot");
    }

    /// Calibration is adopted (÷ 100).
    #[test]
    fn calibration_from_lightroom() {
        let t = "s = { RedHue = 10,\nBlueSaturation = -20,\nShadowTint = 5 }";
        let s = to_settings(&from_catalog(t).unwrap(), &empty_image(), &Settings::default());
        assert_eq!(s.raw.calibration, [0.1, 0.0, 0.0, 0.0, 0.0, -0.2, 0.05]);
    }

    /// `HasCrop` is missing in some catalog entries, Lightroom crops anyway.
    #[test]
    fn crop_without_hascrop() {
        let t = "s = { CropBottom = 0.9,\nCropLeft = 0.2,\nCropTop = 0.1,\nExposure2012 = 0 }";
        let s = to_settings(&from_catalog(t).unwrap(), &empty_image(), &Settings::default());
        assert_ne!(s.crop, Settings::default().crop, "crop taken from the values");
        let t = "s = { HasCrop = false,\nCropLeft = 0.2,\nExposure2012 = 0 }";
        let s = to_settings(&from_catalog(t).unwrap(), &empty_image(), &Settings::default());
        assert_eq!(s.crop, Settings::default().crop, "explicitly without crop stays without");
    }

    /// `CropRight = 0, CropBottom = 0` without the other edges has no area and is no crop
    /// (otherwise the development would only show a single-colored pixel and "fit 100000 %").
    #[test]
    fn crop_without_area_is_ignored() {
        let t = "s = { CropBottom = 0,\nCropConstrainAspectRatio = true,\nCropRight = 0,\nExposure2012 = 0 }";
        let s = to_settings(&from_catalog(t).unwrap(), &empty_image(), &Settings::default());
        assert_eq!(s.crop, Settings::default().crop);
        let t = "s = { HasCrop = true,\nCropBottom = 0,\nCropRight = 0 }";
        let s = to_settings(&from_catalog(t).unwrap(), &empty_image(), &Settings::default());
        assert_eq!(s.crop, Settings::default().crop);
    }

    /// Sliders of a look are added to those of the image (highlights −25 + 10 = −15), curves run in sequence.
    #[test]
    fn look_sliders_add() {
        let t = "s = { Highlights2012 = 10,\nLook = { Amount = 1,\nName = \"Test\",\nParameters = { Highlights2012 = -25,\nContrast2012 = 20,\nSharpness = 90,\nToneCurvePV2012 = { 0,\n0,\n128,\n100,\n255,\n255 },\nVersion = \"16.2\" },\nUUID = \"00000000000000000000000000000000\" } }";
        let w = with_look(&from_catalog(t).unwrap());
        assert_eq!(w["Highlights2012"].number(), Some(-15.0));
        assert_eq!(w["Contrast2012"].number(), Some(20.0));
        assert!(!w.contains_key("Sharpness"), "sharpening is left out");
        let Value::List(k) = &w["ToneCurvePV2012"] else { panic!("curve missing") };
        assert_eq!(k.len(), 34);
        // center of the look's curve: 128 → ≈ 100
        let y = k[17].number().unwrap();
        assert!((y - 100.0).abs() < 8.0, "{y}");
        // half intensity: half way
        let t = t.replace("Amount = 1", "Amount = 0.5");
        let w = with_look(&from_catalog(&t).unwrap());
        assert_eq!(w["Highlights2012"].number(), Some(10.0 - 12.5));
    }

    #[test]
    fn catalog_text() {
        let t = "s = { AutoGrayscaleMix = false,\nBlacks2012 = -5,\nCameraProfile = \"Camera Classic Chrome\",\nExposure2012 = 0.35,\nToneCurvePV2012 = { 0,\n0,\n128,\n140,\n255,\n255 },\nPointColors = {  },\nWhiteBalance = \"As Shot\" }";
        let w = from_catalog(t).unwrap();
        assert_eq!(w["Exposure2012"].number(), Some(0.35));
        assert_eq!(w["CameraProfile"].text(), Some("Camera Classic Chrome"));
        let looks = vec!["x-t30/classic-chrome".to_string()];
        let b = ImageContext { camera_short: "x-t30", looks: &looks, orientation: 0, finished: false, size: [600, 400] };
        let s = to_settings(&w, &b, &Settings::default());
        assert_eq!(s.raw.look, "x-t30/classic-chrome");
        assert!((s.raw.blacks + 0.05).abs() < 1e-6);
        assert_eq!(s.tone_curve.rgb.len(), 3);
    }

    #[test]
    fn xmp() {
        let t = r#"<rdf:Description crs:Exposure2012="+0.50" crs:WhiteBalance="Custom" crs:Temperature="4800" crs:Tint="+7"><crs:ToneCurvePV2012><rdf:Seq><rdf:li>0, 0</rdf:li><rdf:li>255, 255</rdf:li></rdf:Seq></crs:ToneCurvePV2012></rdf:Description>"#;
        let w = from_xmp(t);
        let b = ImageContext { camera_short: "eos-rp", looks: &[], orientation: 0, finished: false, size: [600, 400] };
        let s = to_settings(&w, &b, &Settings::default());
        assert_eq!(s.exposure, 0.5);
        assert_eq!((s.raw.wb_mode.as_str(), s.raw.temperature, s.raw.tint), ("custom", 4800.0, 7.0));
        assert!(s.tone_curve.is_identity());
    }
}

#[cfg(test)]
mod film_reference_tests {
    use super::*;
    #[test]
    fn missing_look_keeps_name_and_package() {
        let text=r#"s = { Look = { UUID = "SYNTHETIC-MISSING", Name = "Synthetic Film", Group = "Synthetic package", Amount = 0.7 } }"#;
        let b=ImageContext {camera_short:"gr3",looks:&[],orientation:0,finished:false,size:[600,400]};
        let s=to_settings(&from_catalog(text).unwrap(),&b,&Settings::default());
        assert_eq!(s.raw.look,"film:SYNTHETIC-MISSING");assert_eq!(s.raw.look_name,"Synthetic Film");assert_eq!(s.raw.look_package,"Synthetic package");assert_eq!(s.profile_amount,0.7);
        let adobe=to_settings(&from_catalog(&text.replace("Synthetic Film","Adobe Color")).unwrap(),&b,&Settings::default());
        assert!(!adobe.raw.look.starts_with("film:"));
    }
}

#[cfg(test)]
mod lua_guard_tests {
    use super::*;

    #[test]
    fn deep_nesting_is_rejected_instead_of_overflowing_the_stack() {
        let deep = format!("s = {}{}", "{".repeat(100_000), "}".repeat(100_000));
        assert!(from_catalog(&deep).is_none());
        let ok = format!("s = {}{}", "{a = ".repeat(10) + "1", "}".repeat(10));
        assert!(from_catalog(&ok).is_some());
        let normal = from_catalog("s = { Exposure2012 = 0.5, ToneCurve = { 0, 0, 255, 255 } }").unwrap();
        assert!(normal.contains_key("Exposure2012"));
    }
}
