//! Develop settings of graineo 1.0 (old JSON keys) load; everything written afterwards has the current keys.
//! The old keys come from `legacy_names`, so no old word stands here.

use develop::export::OutputSize;
use develop::film::FilmSettings;
use develop::Settings;
use legacy_names::settings_keys::{to_old_for_tests, DEVELOP};

fn sample() -> Settings {
    let mut s = Settings::default();
    s.raw.lens_image = Some(true);
    s.film.active = true;
    s.film.look_datasheet = true;
    s.film.layer_exposure = false;
    s.film.grain_balance = false;
    s
}

#[test]
fn settings_in_the_old_format_load() {
    let s = sample();
    let mut v = serde_json::to_value(&s).unwrap();
    to_old_for_tests(&mut v, DEVELOP);
    assert_ne!(v, serde_json::to_value(&s).unwrap(), "the test document must really be old");
    let loaded: Settings = serde_json::from_value(v.clone()).unwrap();
    assert_eq!(loaded, s);
    // also from text, as the library and sidecars deliver it
    assert_eq!(serde_json::from_str::<Settings>(&v.to_string()).unwrap(), s);
}

#[test]
fn settings_are_written_with_current_keys() {
    let v = serde_json::to_value(sample()).unwrap();
    assert_eq!(v["raw"]["lens_image"], true);
    assert_eq!(v["film"]["look_datasheet"], true);
    assert_eq!(v["film"]["layer_exposure"], false);
    assert_eq!(v["film"]["grain_balance"], false);
    // writing again after a load gives the same document
    let again = serde_json::to_value(serde_json::from_value::<Settings>(v.clone()).unwrap()).unwrap();
    assert_eq!(again, v);
}

#[test]
fn recipe_film_settings_in_the_old_format_load() {
    let f = sample().film;
    let mut v = serde_json::to_value(&f).unwrap();
    to_old_for_tests(&mut v, DEVELOP);
    assert_eq!(serde_json::from_value::<FilmSettings>(v).unwrap(), f);
}

#[test]
fn output_size_in_the_old_format_loads() {
    for size in [OutputSize::LongEdge(2048), OutputSize::ShortEdge(1080), OutputSize::BoundingBox { width: 1920, height: 1080 }, OutputSize::Megapixel(6.0)] {
        let mut v = serde_json::to_value(size).unwrap();
        to_old_for_tests(&mut v, DEVELOP);
        assert_eq!(OutputSize::from_json(v).unwrap(), size);
        let current = serde_json::to_value(size).unwrap();
        assert_eq!(OutputSize::from_json(current).unwrap(), size);
    }
}
