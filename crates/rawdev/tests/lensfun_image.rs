//! lensfun for finished images: `GRAINEO_LENS_IMAGE=<jpeg> cargo test -p rawdev --release --test lensfun_image -- --ignored --nocapture`

#[test]
#[ignore]
fn decision() {
    let Ok(p) = std::env::var("GRAINEO_LENS_IMAGE") else { return };
    for f in p.split(',') {
        let doc = rawdev::RawDoc::open(std::path::Path::new(f), &|| true).unwrap().unwrap();
        let b = doc.lens_status();
        let mut s = develop::Settings::default();
        let auto = doc.detail_key(&s);
        s.raw.lens_image = Some(true);
        let on = doc.detail_key(&s);
        let k = doc.correction(on).0;
        println!(
            "{}: {} {} · profile {:?} · {} · automatic {:?} · switched on: correction {}",
            f.rsplit('/').next().unwrap_or(f),
            doc.info.maker,
            doc.info.model,
            b.profile,
            b.reason,
            auto,
            k.as_ref().map_or("none".to_string(), |k| format!("geometry {} vignetting {} zoom {:.3}", k.geometry.is_some(), k.vignette.is_some(), k.zoom))
        );
    }
}
