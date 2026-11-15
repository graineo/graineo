//! Last developed view per image on disk (`previews/developed/<image>.jpg`, 1280 px long edge, with crop and rotation).
//!
//! On the next open it serves as a placeholder until the image is re-rendered: what you last saw appears
//! first (with the settings and initial values), not the camera JPEG, which looks different. Pure cache,
//! can be regenerated at any time.

use std::path::PathBuf;

use eframe::egui::ColorImage;

fn folder() -> PathBuf {
    library::data_dir().join("previews").join("developed")
}

fn path(image_id: i64) -> PathBuf {
    folder().join(format!("{image_id}.jpg"))
}

/// Store the view (JPEG encoding and writing in the background) with the fingerprint of its settings
/// ([`crate::browse::expected`]).
pub fn store(image_id: i64, view: &ColorImage, fingerprint: u64) {
    // photos of a viewed folder (not in the library) have IDs valid only for this session: nothing on disk
    if image_id <= 0 {
        return;
    }
    let (w, h) = (view.size[0] as u32, view.size[1] as u32);
    let mut rgb = Vec::with_capacity(view.pixels.len() * 3);
    for p in &view.pixels {
        rgb.extend_from_slice(&[p.r(), p.g(), p.b()]);
    }
    std::thread::spawn(move || {
        let Some(image_rgb) = image::RgbImage::from_raw(w, h, rgb) else { return };
        let target = path(image_id);
        let _ = std::fs::create_dir_all(folder());
        let part = target.with_extension("teil");
        let Ok(file) = std::fs::File::create(&part) else { return };
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::BufWriter::new(file), 88);
        if enc.encode_image(&image_rgb).is_ok() {
            if std::fs::rename(&part, &target).is_ok() {
                crate::browse::set_developed(image_id, fingerprint);
            }
        } else {
            let _ = std::fs::remove_file(&part);
        }
    });
}

/// Most recently stored view, if any.
pub fn load(image_id: i64) -> Option<ColorImage> {
    if image_id <= 0 {
        return None;
    }
    let b = image::open(path(image_id)).ok()?.to_rgb8();
    Some(ColorImage::from_rgb([b.width() as usize, b.height() as usize], b.as_raw()))
}
