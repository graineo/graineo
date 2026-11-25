//! PROP section: key/value pairs in UTF-16 (camera model, exposure, …).

use crate::container::u32_at;
use crate::error::{corrupt, Result};

#[derive(Debug, Clone, Default)]
pub struct PropList {
    pub pairs: Vec<(String, String)>,
}

impl PropList {
    const HEADER_SIZE: usize = 24;

    pub fn parse(s: &[u8]) -> Result<Self> {
        if s.get(0..4) != Some(b"SECp") {
            return corrupt("PROP-Sektion ohne SECp-Kennung");
        }
        let num = u32_at(s, 8)? as usize;
        // every pair needs 8 bytes in the table; a larger count only occurs in damaged files
        if num > s.len().saturating_sub(Self::HEADER_SIZE) / 8 {
            return corrupt(format!("PROP-Sektion mit unplausibler Zahl {num}"));
        }
        let table = Self::HEADER_SIZE;
        let chars_start = table + num * 8;
        let chars: Vec<u16> = s[chars_start.min(s.len())..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let read_str = |off: usize| -> String {
            let end = chars[off.min(chars.len())..]
                .iter()
                .position(|&c| c == 0)
                .map(|p| off + p)
                .unwrap_or(chars.len());
            String::from_utf16_lossy(&chars[off.min(chars.len())..end])
        };
        let mut pairs = Vec::with_capacity(num);
        for i in 0..num {
            let name = u32_at(s, table + i * 8)? as usize;
            let value = u32_at(s, table + i * 8 + 4)? as usize;
            pairs.push((read_str(name), read_str(value)));
        }
        Ok(Self { pairs })
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
}

/// Name of the white balance for the CAMF code `WhiteBalance` (Quattro; names as in the
/// CAMF lists `WhiteBalanceGains`/`WhiteBalanceColorCorrections`).
pub fn quattro_wb_name(code: u32) -> &'static str {
    match code {
        2 => "Sunlight",
        3 => "Shade",
        4 => "Overcast",
        5 => "Incandescent",
        6 => "Fluorescent",
        7 => "Flash",
        8 => "Custom",
        11 => "ColorTemp",
        10 => "AutoLSP",
        _ => "Auto",
    }
}

/// Colour mode name from the MakerNote (PictureMode) → CAMF key as for Merrill (`CM_DESC`).
pub fn picture_mode_key(mode: &str) -> String {
    match mode {
        "Fov Classic Blue" | "FOV Classic Blue" => "FCBlue".into(),
        "Fov Classic Yellow" | "FOV Classic Yellow" => "FCYellow".into(),
        "Sunset Red" => "SunsetRed".into(),
        "Forest Green" => "ForestGreen".into(),
        "Teal and Orange" => "TealAndOrange".into(),
        m => m.to_string(),
    }
}

impl PropList {
    /// Property list for files without a PROP section (Quattro) from EXIF and CAMF, with the
    /// keys of the Merrill PROP so that display and calibration stay the same.
    pub fn synthesize(exif: crate::exif::JpegExif, camf: Option<&crate::Camf>, rotation: Option<u32>) -> Self {
        let mut pairs: Vec<(String, String)> = Vec::new();
        let mut put = |k: &str, v: Option<String>| {
            if let Some(v) = v {
                pairs.push((k.to_string(), v));
            }
        };
        let scalar = |name: &str| camf.and_then(|c| c.matrix(name).ok()).and_then(|m| m.as_f64().first().copied());
        let model = exif.model.clone().map(|m| if m.to_ascii_uppercase().starts_with("SIGMA") { m } else { format!("SIGMA {m}") });
        put("CAMMANUF", exif.make.clone().or(Some("SIGMA".into())));
        put("CAMMODEL", model);
        put("CAMSERIAL", exif.serial.clone());
        let t = exif.exposure_time.or_else(|| scalar("CaptureShutter").map(|us| us / 1e6));
        put("SHUTTER", t.map(|t| format!("{t}")));
        put("SH_DESC", t.map(|t| if t > 0.0 && t < 0.3 { format!("1/{}", (1.0 / t).round()) } else { format!("{}", (t * 10.0).round() / 10.0) }));
        let f = exif.f_number.or_else(|| scalar("CaptureAperture"));
        put("APERTURE", f.map(|f| format!("{f}")));
        put("AP_DESC", f.map(|f| format!("{}", (f * 10.0).round() / 10.0)));
        let iso = exif.iso.map(f64::from).or_else(|| scalar("CaptureISO"));
        put("ISO", iso.map(|v| format!("{}", v.round())));
        put("FLENGTH", exif.focal_length.map(|v| format!("{}", (v * 10.0).round() / 10.0)));
        put("FLEQ35MM", exif.focal_length_35mm.map(|v| v.to_string()));
        put("EXPCOMP", exif.exposure_bias.or_else(|| scalar("CaptureExpComp")).map(|v| format!("{v:+.2}")));
        // the light-source-priority auto white balance is stored as "Auto" (1) in the CAMF, only the MakerNote says
        // "Auto(Natural)"; the camera maker's software develops it with its own gains (`AutoLSP`)
        let natural = exif.white_balance.as_deref().is_some_and(|w| w.replace(' ', "").eq_ignore_ascii_case("Auto(Natural)"));
        let wb = scalar("WhiteBalance").map(|c| if c as u32 == 1 && natural { "Auto(Natural)".to_string() } else { quattro_wb_name(c as u32).to_string() });
        put("WB_DESC", wb);
        put("CM_DESC", exif.picture_mode.as_deref().map(picture_mode_key));
        put("IMAGERTEMP", scalar("SensorTemperature").map(|v| format!("{}", v.round())));
        put("COLORSPACE", exif.color_space.map(|c| if c == 1 { "sRGB".to_string() } else { "AdobeRGB".to_string() }));
        // Orientation: file header (degrees clockwise), otherwise EXIF orientation
        let rot = rotation.filter(|r| r % 90 == 0 && *r < 360).or(match exif.orientation {
            Some(6) => Some(90),
            Some(3) => Some(180),
            Some(8) => Some(270),
            Some(_) => Some(0),
            None => None,
        });
        put("ROTATION", rot.map(|r| r.to_string()));
        put("TIME", exif.date_time.clone());
        Self { pairs }
    }
}
