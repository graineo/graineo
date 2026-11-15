//! Film profile references are kept intact even when no source is installed.
use develop::Settings;

pub fn remember_reference(s: &mut Settings) {
    if let Some(id) = s.raw.look.strip_prefix("film:") {
        if let Some(p) = raw::film_profile::by_uuid(id) {
            s.raw.look_name = p.name;
            s.raw.look_package = p.group;
        }
    } else {
        s.raw.look_name.clear();
        s.raw.look_package.clear();
    }
}
pub fn label(s: &Settings) -> String {
    let name = if s.raw.look_name.is_empty() {
        s.raw.look.strip_prefix("film:").unwrap_or(&s.raw.look)
    } else {
        &s.raw.look_name
    };
    if s.raw.look_package.is_empty() {
        name.into()
    } else {
        format!("{name} ({})", s.raw.look_package)
    }
}
fn active(s: &Settings) -> Option<&str> {
    if s.film.is_active() {
        None
    } else {
        s.raw.look.strip_prefix("film:")
    }
}
/// Quick check for the UI. The full table check happens at render/export time.
pub fn missing(s: &Settings) -> Option<String> {
    let id = active(s)?;
    if raw::film_profile::by_uuid(id).is_none_or(|p| !p.available)
        || raw::film_profile::load_failed(id)
    {
        Some(format!("Look nicht verfügbar: {}", label(s)))
    } else {
        None
    }
}
/// Full check in the background, including table contents. No silent neutral export.
pub fn check(s: &Settings) -> Result<(), String> {
    if let Some(id) = active(s) {
        if raw::film_profile::by_uuid(id)
            .and_then(|p| p.look())
            .is_none()
        {
            return Err(format!("Look nicht verfügbar: {}", label(s)));
        }
    }
    Ok(())
}
/// Only after explicit confirmation: develop neutrally for this export job.
/// The stored profile reference is left untouched.
pub fn for_export(s: &Settings, skip_missing: bool) -> Result<Settings, String> {
    match check(s) {
        Ok(()) => Ok(s.clone()),
        Err(_) if skip_missing => {
            let mut t = s.clone();
            t.raw.look = "neutral".into();
            Ok(t)
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_reference_survives_json_and_confirmed_export() {
        let mut s = Settings::default();
        s.raw.look = "film:MISSING-SYNTHETIC".into();
        s.raw.look_name = "Synthetic Portra".into();
        s.raw.look_package = "Synthetic package".into();
        let before = s.clone();
        remember_reference(&mut s);
        assert_eq!(s, before);
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("lookName") && json.contains("lookPackage"));
        let copy: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(copy, s);
        assert!(missing(&s)
            .unwrap()
            .contains("Synthetic Portra (Synthetic package)"));
        assert!(for_export(&s, false).is_err());
        assert_eq!(for_export(&s, true).unwrap().raw.look, "neutral");
        assert_eq!(s, before);
    }
    #[test]
    fn other_active_film_does_not_overwrite_reference() {
        let mut s = Settings::default();
        s.raw.look = "film:MISSING-SYNTHETIC".into();
        s.film.active = true;
        // No actual film data are required to test the persisted reference itself.
        remember_reference(&mut s);
        assert_eq!(s.raw.look, "film:MISSING-SYNTHETIC");
        s.raw.look = "neutral".into();
        remember_reference(&mut s);
        assert!(s.raw.look_name.is_empty());
    }
}
