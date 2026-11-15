//! Lens profile files of graineo 1.0 (old JSON keys) load; new ones are written with the current keys.

use legacy_names::settings_keys::{to_old_for_tests, LENS_PROFILE};
use lensprof::model::{Entry, Profile};
use lensprof::OwnProfile;

fn sample() -> OwnProfile {
    let p = Profile { name: "T".into(), crop: 1.0, entries: vec![Entry { focal_length: 50.0, distance: 1000.0, av: 4.0, geo: None, ca: None, vig: None }] };
    OwnProfile::new("SIGMA", "sd Quattro", "", "graineo-Messung", p)
}

#[test]
fn own_profile_in_the_old_format_loads() {
    let p = sample();
    let current = serde_json::to_value(&p).unwrap();
    assert_eq!(current["maker"], "SIGMA");
    assert_eq!(current["profile"]["entries"][0]["focal_length"], 50.0);
    let mut old = current.clone();
    to_old_for_tests(&mut old, LENS_PROFILE);
    assert_ne!(old, current);
    let loaded = OwnProfile::from_json(&old.to_string(), "stem").unwrap();
    assert_eq!((loaded.maker.as_str(), loaded.model.as_str(), loaded.profile.entries.len()), ("SIGMA", "sd Quattro", 1));
    assert_eq!(loaded.profile.entries[0].focal_length, 50.0);
    // current files load too
    assert_eq!(OwnProfile::from_json(&current.to_string(), "stem").unwrap().maker, "SIGMA");
}
