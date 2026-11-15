//! Regression: the real SFace model (OpenCV Zoo) must load and give a 128-number vector.
//! Runs only if the model file is present locally (`GRAINEO_FACE_MODEL_TEST` or the default module folder);
//! the file is only read.

fn model_file() -> Option<std::path::PathBuf> {
    if let Some(p) = std::env::var_os("GRAINEO_FACE_MODEL_TEST") {
        return Some(p.into());
    }
    let home = std::env::var_os("HOME")?;
    let p = std::path::Path::new(&home).join("Library/Application Support/graineo/modules/gesichter").join(faces::sface::FILE_NAME);
    p.exists().then_some(p)
}

#[test]
fn real_sface_model_loads_and_computes() {
    let Some(p) = model_file() else {
        eprintln!("SFace model not present, test skipped");
        return;
    };
    let m = faces::sface::Sface::load(&p).expect("the real SFace model must load");
    let img: Vec<u8> = (0..112 * 112 * 3).map(|i| (i * 7 % 251) as u8).collect();
    let v = m.embedding(&img).expect("Einbettung");
    assert_eq!(v.len(), faces::sface::EMBEDDING_LEN);
    assert!(v.iter().all(|x| x.is_finite()));
}
