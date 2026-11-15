//! Lightroom/Camera Raw settings (XMP, namespace `crs`) from our settings,
//! embedded into the DNG so that Lightroom opens the file "pre-developed".
//!
//! What exists in Lightroom is transferred (exposure, contrast, highlights, shadows, presence,
//! tone curve, colour mixer, color grading, effects, fringes, crop, straightening, profile). Foveon-specific
//! things (Sigma tone curve, Fill Light, layer mix) do not exist in Lightroom; the layer mix
//! is baked into the data for the monochrome DNG. Other programs compute differently; the look is
//! similar, not identical.

use crate::{geometry, Calibration, Settings};

fn num(v: f64) -> String {
    format!("{}", (v.round() as i64))
}

/// Ratio of the SPP sharpening amount to the amount at sharpness 0 (base ≈ 1).
fn sigma_sharpness_factor(slider: f32) -> f64 {
    let s = crate::sharpness::quantize(slider);
    if s < 0.0 { 1.0 + s / 2.0 } else { 1.0 + s }
}

fn pct(v: f32) -> String {
    num((v as f64 * 100.0).clamp(-100.0, 100.0))
}

/// XMP packet for settings `s`. `profile`: name of the embedded DNG profile (e.g. "Portrait"),
/// `None` = reference standard.
pub fn lightroom_xmp(s: &Settings, cal: &Calibration, profile: Option<&str>, monochrome_data: bool) -> String {
    let base: Vec<(&str, String)> = vec![
        ("crs:Version", "15.0".into()),
        ("crs:ProcessVersion", "11.0".into()),
        ("crs:HasSettings", "True".into()),
        ("crs:WhiteBalance", "As Shot".into()),
        ("crs:Exposure2012", format!("{:+.2}", s.exposure.clamp(-5.0, 5.0))),
        ("crs:Contrast2012", num((s.contrast * 50.0).clamp(-100.0, 100.0))),
        ("crs:Highlights2012", num((s.highlight * 50.0).clamp(-100.0, 100.0))),
        ("crs:Shadows2012", num((s.shadow * 50.0).clamp(-100.0, 100.0))),
        ("crs:Clarity2012", pct(s.clarity)),
        ("crs:Vibrance", pct(s.vibrance)),
        ("crs:Saturation", pct(s.saturation)),
        // Sharpening: the DNG data is unsharpened. Sigma's sharpness slider scales the
        // base sharpening (SPP: amount × (1 + s) or × (1 + s/2) for s < 0)
        // → relative to Lightroom's default of 25; "additional sharpening" is added.
        ("crs:Sharpness", num((25.0 * sigma_sharpness_factor(s.sigma_sharpness) + s.sharpen as f64 * 50.0).clamp(0.0, 150.0))),
        ("crs:SharpenRadius", format!("{:+.1}", s.sharpen_radius.clamp(0.5, 3.0))),
        ("crs:SharpenEdgeMasking", pct(s.sharpen_mask)),
        ("crs:LuminanceSmoothing", "0".into()),
        ("crs:ColorNoiseReduction", "0".into()),
        ("crs:PostCropVignetteAmount", pct(s.vignette)),
        ("crs:PostCropVignetteMidpoint", num(s.vignette_mid as f64 * 100.0)),
        ("crs:PostCropVignetteStyle", "1".into()),
        ("crs:GrainAmount", num(s.grain_amount as f64 * 100.0)),
        ("crs:GrainSize", num((s.grain_size as f64 * 25.0).clamp(0.0, 100.0))),
        ("crs:GrainFrequency", num(s.grain_rough as f64 * 100.0)),
        // Remove colour fringes (lens corrections → manual): amount 0 … 20, hue 0 … 100
        ("crs:DefringePurpleAmount", num((s.defringe_purple_amount as f64 * 20.0).clamp(0.0, 20.0))),
        ("crs:DefringePurpleHueLo", num(s.defringe_purple_hue[0].min(s.defringe_purple_hue[1]) as f64)),
        ("crs:DefringePurpleHueHi", num(s.defringe_purple_hue[0].max(s.defringe_purple_hue[1]) as f64)),
        ("crs:DefringeGreenAmount", num((s.defringe_green_amount as f64 * 20.0).clamp(0.0, 20.0))),
        ("crs:DefringeGreenHueLo", num(s.defringe_green_hue[0].min(s.defringe_green_hue[1]) as f64)),
        ("crs:DefringeGreenHueHi", num(s.defringe_green_hue[0].max(s.defringe_green_hue[1]) as f64)),
        // Chromatic aberration (lens corrections): Lightroom has no Sigma profile for
        // linear DNG, but "remove chromatic aberration" (automatic lateral CA)
        ("crs:AutoLateralCA", if s.ca_mode != crate::lens::CaMode::Off && !s.monochrome { "1" } else { "0" }.into()),
        ("crs:ConvertToGrayscale", if s.monochrome || monochrome_data { "True" } else { "False" }.into()),
    ];
    let mut a: Vec<(String, String)> = base.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    // Colour mixer: the same eight ranges
    let bands = ["Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta"];
    let keys: [[&str; 8]; 3] = [
        bands.map(|b| match b {
            "Red" => "crs:HueAdjustmentRed",
            "Orange" => "crs:HueAdjustmentOrange",
            "Yellow" => "crs:HueAdjustmentYellow",
            "Green" => "crs:HueAdjustmentGreen",
            "Aqua" => "crs:HueAdjustmentAqua",
            "Blue" => "crs:HueAdjustmentBlue",
            "Purple" => "crs:HueAdjustmentPurple",
            _ => "crs:HueAdjustmentMagenta",
        }),
        bands.map(|b| match b {
            "Red" => "crs:SaturationAdjustmentRed",
            "Orange" => "crs:SaturationAdjustmentOrange",
            "Yellow" => "crs:SaturationAdjustmentYellow",
            "Green" => "crs:SaturationAdjustmentGreen",
            "Aqua" => "crs:SaturationAdjustmentAqua",
            "Blue" => "crs:SaturationAdjustmentBlue",
            "Purple" => "crs:SaturationAdjustmentPurple",
            _ => "crs:SaturationAdjustmentMagenta",
        }),
        bands.map(|b| match b {
            "Red" => "crs:LuminanceAdjustmentRed",
            "Orange" => "crs:LuminanceAdjustmentOrange",
            "Yellow" => "crs:LuminanceAdjustmentYellow",
            "Green" => "crs:LuminanceAdjustmentGreen",
            "Aqua" => "crs:LuminanceAdjustmentAqua",
            "Blue" => "crs:LuminanceAdjustmentBlue",
            "Purple" => "crs:LuminanceAdjustmentPurple",
            _ => "crs:LuminanceAdjustmentMagenta",
        }),
    ];
    for (k, row) in keys.iter().enumerate() {
        for (i, key) in row.iter().enumerate() {
            a.push((key.to_string(), pct(s.hsl[i][k])));
        }
    }
    // Color Grading
    for (prefix, [h, sat]) in [("Shadow", s.grade_shadows), ("Midtone", s.grade_midtones), ("Highlight", s.grade_highlights)] {
        a.push((format!("crs:ColorGrade{prefix}Hue"), num(h as f64)));
        a.push((format!("crs:ColorGrade{prefix}Sat"), num(sat as f64 * 100.0)));
    }
    a.push(("crs:ColorGradeBalance".to_string(), pct(s.grade_balance)));
    a.push(("crs:ColorGradeBlending".to_string(), "50".into()));
    if let Some(p) = profile.filter(|_| !monochrome_data) {
        a.push(("crs:CameraProfile".to_string(), p.to_string()));
    }
    // Crop (in the upright image, normalised) and straightening
    let [cx, cy, w, h] = s.crop;
    let cropped = s.crop != Settings::default().crop || s.crop_angle != 0.0;
    // Perspective (transform, −100 … 100). The reference model takes
    // the focal length into account, ours uses a fixed keystone strength: direction matches, amount only
    // approximately.
    if s.perspective_v != 0.0 || s.perspective_h != 0.0 {
        a.push(("crs:PerspectiveVertical".to_string(), format!("{:.0}", s.perspective_v * 100.0)));
        a.push(("crs:PerspectiveHorizontal".to_string(), format!("{:.0}", s.perspective_h * 100.0)));
    }
    a.push(("crs:HasCrop".to_string(), if cropped { "True" } else { "False" }.into()));
    if cropped {
        a.push(("crs:CropLeft".to_string(), format!("{:.6}", (cx - w / 2.0).clamp(0.0, 1.0))));
        a.push(("crs:CropTop".to_string(), format!("{:.6}", (cy - h / 2.0).clamp(0.0, 1.0))));
        a.push(("crs:CropRight".to_string(), format!("{:.6}", (cx + w / 2.0).clamp(0.0, 1.0))));
        a.push(("crs:CropBottom".to_string(), format!("{:.6}", (cy + h / 2.0).clamp(0.0, 1.0))));
        a.push(("crs:CropAngle".to_string(), format!("{:.3}", s.crop_angle)));
        a.push(("crs:CropConstrainToWarp".to_string(), "0".into()));
    }
    let orientation = match geometry::quarters(s, cal.orientation) {
        1 => 6,
        2 => 3,
        3 => 8,
        _ => 1,
    };
    a.push(("tiff:Orientation".to_string(), orientation.to_string()));

    // Tone curve: Lightroom expects point lists (0 … 255) as rdf:Seq elements
    let children = tone_curve_xmp(&s.tone_curve);
    if !children.is_empty() {
        a.push(("crs:ToneCurveName2012".to_string(), "Custom".into()));
    }
    let attrs: String = a.iter().map(|(k, v)| format!("\n   {k}=\"{v}\"")).collect();
    let body = if children.is_empty() { "/>".to_string() } else { format!(">{children}\n  </rdf:Description>") };
    format!(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n\
<x:xmpmeta xmlns:x=\"adobe:ns:meta/\" x:xmptk=\"graineo\">\n\
 <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n\
  <rdf:Description rdf:about=\"\"\n\
   xmlns:crs=\"http://ns.adobe.com/camera-raw-settings/1.0/\"\n\
   xmlns:tiff=\"http://ns.adobe.com/tiff/1.0/\"{attrs}{body}\n\
 </rdf:RDF>\n\
</x:xmpmeta>\n\
<?xpacket end=\"w\"?>"
    )
}

/// Point curves for Lightroom (process version 2012): `crs:ToneCurvePV2012` (RGB) and
/// `…Red/Green/Blue`, each point "x, y" in 0 … 255. Empty if the curve has no effect;
/// otherwise all four channels are written (diagonal as 0,0 → 255,255).
fn tone_curve_xmp(c: &crate::ToneCurve) -> String {
    if c.is_identity() {
        return String::new();
    }
    let mut out = String::new();
    for (i, suffix) in ["", "Red", "Green", "Blue"].iter().enumerate() {
        let p = c.channel(i);
        let pts: Vec<[f32; 2]> = if p.len() < 2 { vec![[0.0, 0.0], [1.0, 1.0]] } else { p.clone() };
        let li: String = pts
            .iter()
            .map(|q| format!("\n     <rdf:li>{}, {}</rdf:li>", (q[0].clamp(0.0, 1.0) * 255.0).round() as i32, (q[1].clamp(0.0, 1.0) * 255.0).round() as i32))
            .collect();
        out.push_str(&format!("\n   <crs:ToneCurvePV2012{suffix}>\n    <rdf:Seq>{li}\n    </rdf:Seq>\n   </crs:ToneCurvePV2012{suffix}>"));
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn tone_curve_as_seq() {
        assert!(super::tone_curve_xmp(&crate::ToneCurve::default()).is_empty());
        let c = crate::ToneCurve { rgb: vec![[0.0, 0.0], [0.25, 0.17], [0.75, 0.85], [1.0, 1.0]], ..Default::default() };
        let x = super::tone_curve_xmp(&c);
        eprintln!("{x}");
        assert!(x.contains("<crs:ToneCurvePV2012>") && x.contains("<rdf:li>64, 43</rdf:li>") && x.contains("<crs:ToneCurvePV2012Blue>"));
    }
}
