//! "Save look as LUT": the look chosen for a photo as a 3D LUT (the same table the development uses).
use crate::doc::RawDoc;
use develop::Settings;

impl RawDoc {
    /// Whether the settings choose a look whose color mapping can be exported (a film is exported separately).
    pub fn look_exportable(&self, s: &Settings) -> bool {
        !s.film.is_active() && self.look_name(s).is_some()
    }

    /// `.cube` text (33³, sRGB → sRGB) of the look chosen in `s`, mixed with its intensity (`profile_amount`).
    /// Only the color mapping of the look: exposure, white balance, tone curve, saturation, local adjustments,
    /// sharpening, noise reduction and grain are not in it.
    pub fn look_cube(&self, s: &Settings, title: &str) -> Result<String, String> {
        if s.film.is_active() {
            return Err("Ein Film ist gewählt (kein Kamera-Look)".into());
        }
        if self.look_name(s).is_none() {
            return Err("Kein Look gewählt".into());
        }
        let look = self.look_for(s).ok_or("Der Look ist nicht verfügbar")?;
        look.cube_text(title, s.profile_amount.clamp(0.0, 1.0))
    }
}
