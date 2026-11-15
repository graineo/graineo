//! The image face boxes refer to: the photo upright as the library shows it (the file's own orientation plus a
//! rotation from Lightroom) and, for photos whose shape was changed in Apple Photos (cropped, straightened, rotated),
//! Apple's current version – Apple's own boxes refer to it. Face search, face images and crops all load through here,
//! so box and pixels always fit together.

use std::path::{Path, PathBuf};

use image::RgbImage;

/// Where the pixels of a photo are and how it stands.
#[derive(Clone, Debug)]
pub struct Frame {
    pub path: PathBuf,
    pub format: String,
    /// orientation according to the library (EXIF 1–8), `None` = as the file says
    pub orientation: Option<i64>,
    /// size of the upright photo according to the library, if known
    pub upright: Option<(u32, u32)>,
    /// preview cache key (with the library orientation)
    pub key: u64,
}

impl Frame {
    /// Width / height of the upright photo, if known.
    pub fn aspect(&self) -> Option<f64> {
        self.upright.map(|(w, h)| w as f64 / h as f64)
    }
}

/// Upright size from the stored size and the library orientation.
pub fn upright_size(w: Option<i64>, h: Option<i64>, orientation: Option<i64>) -> Option<(u32, u32)> {
    match (w, h) {
        (Some(w), Some(h)) if w > 0 && h > 0 => Some(if matches!(orientation, Some(5..=8)) { (h as u32, w as u32) } else { (w as u32, h as u32) }),
        _ => None,
    }
}

/// Frame of an image (photo row of the library).
pub fn of(db: &crate::db::Db, image: i64) -> Result<Frame, String> {
    let fe = |e: rusqlite::Error| e.to_string();
    let (format, folder, name, size, mtime, orientation, w, h): (String, i64, String, Option<i64>, Option<i64>, Option<i64>, Option<i64>, Option<i64>) = db
        .conn
        .query_row(
            "SELECT d.format, d.folder, d.name, d.size, d.mtime, d.orientation, d.width, d.height FROM image b JOIN file d ON d.id=b.file WHERE b.id=?1",
            [image],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?)),
        )
        .map_err(fe)?;
    let dir: String = db
        .conn
        .query_row("SELECT s.path || o.path FROM folder o JOIN root_folder s ON s.id = o.root WHERE o.id = ?1", [folder], |r| r.get(0))
        .map_err(fe)?;
    let path_text = format!("{dir}{name}");
    let key = crate::preview::cache_key_with_orientation(crate::preview::cache_key(&path_text, size.unwrap_or(0), mtime.unwrap_or(0)), orientation);
    Ok(Frame { path: PathBuf::from(path_text), format, orientation, upright: upright_size(w, h, orientation), key })
}

/// Do two shapes (width / height) agree (2 % tolerance: embedded previews are sometimes a few pixels off)?
pub fn same_shape(a: f64, b: f64) -> bool {
    (a - b).abs() <= 0.02 * b.max(1e-6)
}

/// The photo in the frame of its face boxes, at most `max` pixels edge length.
pub fn load(f: &Frame, max: u32) -> Result<RgbImage, String> {
    match apple_version(f) {
        AppleVersion::Current(p) => {
            let format = crate::format_from_extension(&p.to_string_lossy()).unwrap_or("JPG");
            Ok(crate::preview::load(&p, format, max)?.0)
        }
        AppleVersion::Missing => Err("bearbeitete Fassung aus Apple Fotos nicht auf diesem Mac".into()),
        AppleVersion::Original => {
            let (img, _) = crate::preview::load(&f.path, &f.format, max)?;
            Ok(crate::preview::rotate(img, crate::preview::extra_rotation(&f.path, &f.format, f.orientation)))
        }
    }
}

/// The photo as it is, without regard to the frame (the whole photo instead of an uncertain crop).
pub fn load_plain(f: &Frame, max: u32) -> Result<RgbImage, String> {
    Ok(crate::preview::load(&f.path, &f.format, max)?.0)
}

enum AppleVersion {
    /// not from Apple Photos, or the original has the shape of the photo (or is not on this Mac – then
    /// [`crate::preview::load`] takes Photos' preview, which shows the current version)
    Original,
    /// cropped or rotated in Photos: Apple's current version
    Current(PathBuf),
    /// cropped or rotated in Photos, but no current version on this Mac
    Missing,
}

/// Apple Photos: the original is on this Mac, but the photo was cropped or rotated in Photos (the original's upright
/// size differs from the size Photos gives for the photo, which is that of the current version) → Apple's current
/// version: the rendered full size version if it is on this Mac, else Photos' preview (1024 px).
fn apple_version(f: &Frame) -> AppleVersion {
    if !apple_edited_shape(f) {
        return AppleVersion::Original;
    }
    let library = f.path.ancestors().find(|a| a.extension().is_some_and(|e| e == "photoslibrary"));
    let uuid = crate::apple_photos::uuid_from_path(&f.path);
    match library.zip(uuid).and_then(|(l, u)| crate::apple_photos::render_for(l, &u).or_else(|| crate::apple_photos::preview_for(l, &u))) {
        Some(p) => AppleVersion::Current(p),
        None => AppleVersion::Missing,
    }
}


/// Does a box (normalized, origin top left) lie mostly in the photo? At least half of its area must be inside, and it
/// must have a size. A box outside belongs to another frame (found on another image than the photo, e.g. a crop that
/// is no longer there): it is not drawn and gets the whole photo as face image instead of a crop of the wrong place.
pub fn box_in_photo(x: f64, y: f64, b: f64, h: f64) -> bool {
    if !(b > 0.0 && h > 0.0 && x.is_finite() && y.is_finite()) {
        return false;
    }
    let w = (x + b).min(1.0) - x.max(0.0);
    let hh = (y + h).min(1.0) - y.max(0.0);
    w > 0.0 && hh > 0.0 && w * hh >= 0.5 * b * h
}

/// Does the loaded image have the shape of the photo (or is the shape unknown)?
pub fn fits(f: &Frame, img: &RgbImage) -> bool {
    f.aspect().is_none_or(|a| same_shape(img.width() as f64 / img.height().max(1) as f64, a))
}

/// Was this photo's face search (or a face image made before [`crate::face_index::THUMB_REVISION`]) done on another
/// image than the frame of its boxes? Lightroom rotated it without changing the file (`Some(quarter turns)`), or its
/// shape was changed in Apple Photos while the original was on this Mac (`Some(0)`). Reads only file properties.
pub fn misframed(f: &Frame) -> Option<u8> {
    if apple_edited_shape(f) {
        return Some(0);
    }
    match library_turn(&f) {
        0 => None,
        q => Some(q),
    }
}

/// Quarter turns between the file as it stands and the library (a rotation from Lightroom only). A photo the library
/// shows unturned is read only on this Mac (turned back in Lightroom against its file: rare – it spares reading every
/// file on a network drive).
fn library_turn(f: &Frame) -> u8 {
    match f.orientation {
        None => 0,
        Some(1) if !crate::platform::local(&f.path) => 0,
        _ => crate::preview::extra_rotation(&f.path, &f.format, f.orientation),
    }
}

/// Apple photo whose original on this Mac has another shape than the photo (also when no current version is at hand).
fn apple_edited_shape(f: &Frame) -> bool {
    if f.format == "VIDEO" || !crate::apple_photos::in_photos_library(&f.path) || !f.path.is_file() {
        return false;
    }
    let (Some((w, h)), Some(p)) = (f.upright, crate::imageio::properties(&f.path)) else { return false };
    let (Some(pw), Some(ph)) = (p.width, p.height) else { return false };
    let size = if matches!(p.orientation, Some(5..=8)) { (ph, pw) } else { (pw, ph) };
    size != (w, h)
}

/// Revision of the boxes of graineo's own face search ([`repair`]).
pub const REVISION: &str = "1";

/// What [`repair`] did.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Repair {
    /// photos searched again
    pub images: usize,
    /// boxes turned into the frame (decided in graineo, rotated in Lightroom only)
    pub turned: usize,
    /// boxes without a decision removed (found again by the next search, in the right frame)
    pub removed: usize,
    /// boxes joined with an Apple face that take Apple's box
    pub from_apple: usize,
    /// boxes decided in graineo on cropped photos: where the crop lay is unknown, they stay as they are
    pub kept: usize,
    /// photos whose file cannot be reached now (not looked at)
    pub unreachable: usize,
}

/// Until this time (08.11.2026) a photo kept elsewhere could be represented in the library by a JPEG exported from
/// Lightroom; the face search then looked at that JPEG, which carries Lightroom's crop.
const REPRESENTED_UNTIL: i64 = 1_794_142_262;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Misframe {
    /// rotated in Lightroom only: quarter turns from the file's frame to the library's
    Turned(u8),
    /// cropped or rotated in Apple Photos, searched in the original
    Apple,
    /// cropped in Lightroom, searched in the exported JPEG that represented it
    Cropped,
}

/// Does Lightroom crop the photo (crop edges or angle in its settings)?
fn lightroom_cropped(db: &crate::db::Db, image: i64) -> bool {
    let text: Option<String> = db.conn.query_row("SELECT ds.lr_develop FROM develop_state ds JOIN image b ON b.file = ds.file WHERE b.id = ?1 AND ds.copy = ''", [image], |r| r.get(0)).ok().flatten();
    let Some(t) = text else { return false };
    let value = |key: &str, default: f64| -> f64 {
        t.find(&format!("{key} = ")).and_then(|i| t[i + key.len() + 3..].split(|c: char| c == ',' || c.is_whitespace()).next()?.parse().ok()).unwrap_or(default)
    };
    value("CropLeft", 0.0) > 0.001 || value("CropTop", 0.0) > 0.001 || value("CropRight", 1.0) < 0.999 || value("CropBottom", 1.0) < 0.999 || value("CropAngle", 0.0).abs() > 0.01
}

/// When was the original of an Apple photo put on this Mac (its file time, Unix seconds)?
fn file_time(p: &Path) -> i64 {
    std::fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(i64::MAX, |d| d.as_secs() as i64)
}

/// Once per [`REVISION`], before the face search: graineo's own search used to look at another image than the one
/// its boxes are meant for – on a photo rotated in Lightroom only (file unchanged) the file as it lies, on a photo
/// cropped or rotated in Apple Photos whose original was on this Mac the original, on a RAW cropped in Lightroom that
/// was represented by an exported JPEG (until 08.11.2026) that JPEG. Its boxes lie in another frame than the photo's
/// (and Lightroom's boxes on it got no features). Such photos are searched again: boxes without a decision go (the
/// search finds them again in the right frame), boxes decided in graineo are turned along (rotation) or stay
/// (crops: unknown where), boxes joined with an Apple face take Apple's box. Photos whose file cannot be reached now (network drive) are remembered and looked at in a
/// later run; boxes found or changed since the first run are never touched again.
pub fn repair(db: &crate::db::Db) -> Result<Option<Repair>, String> {
    use rusqlite::params;
    let pending: Option<Vec<i64>> = db.info("face_frames_open").and_then(|s| serde_json::from_str(&s).ok());
    let done = db.info("face_frames").as_deref() == Some(REVISION);
    if done && pending.as_ref().is_none_or(|p| p.is_empty()) {
        return Ok(None);
    }
    let fe = |e: rusqlite::Error| e.to_string();
    // boxes found from here on are in the right frame
    let since: i64 = match db.info("face_frames_since").and_then(|s| s.parse().ok()) {
        Some(t) => t,
        None => {
            let t = crate::now();
            db.set_info("face_frames_since", &t.to_string()).map_err(|e| e.to_string())?;
            t
        }
    };
    let mut images: Vec<(i64, i64)> = {
        let mut st = db.conn.prepare("SELECT image, max(coalesce(detected, 0)) FROM face WHERE source='graineo' GROUP BY image").map_err(fe)?;
        let r = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map_err(fe)?.collect::<rusqlite::Result<_>>().map_err(fe)?;
        r
    };
    if done {
        let p: std::collections::HashSet<i64> = pending.unwrap_or_default().into_iter().collect();
        images.retain(|(i, _)| p.contains(i));
    }
    let mut report = Repair::default();
    let mut later: Vec<i64> = Vec::new();
    let mut apple_boxes: Option<std::collections::HashMap<String, (f32, f32, f32, f32)>> = None;
    for (image, found) in images {
        if found >= since {
            continue;
        }
        let Ok(f) = of(db, image) else { continue };
        let apple = crate::apple_photos::in_photos_library(&f.path);
        let cropped = !apple && found < REPRESENTED_UNTIL && f.format != "JPG" && lightroom_cropped(db, image);
        // only possible cases read the file (see `library_turn`)
        let may_turn = match f.orientation {
            None => false,
            Some(1) => crate::platform::local(&f.path),
            _ => true,
        };
        if !(apple || cropped || may_turn) {
            continue;
        }
        if !f.path.exists() {
            // Apple Photos without the original on this Mac: the search took Photos' preview, the current version
            if !apple {
                report.unreachable += 1;
                later.push(image);
            }
            continue;
        }
        let kind = if apple {
            if !(apple_edited_shape(&f) && found >= file_time(&f.path)) {
                continue;
            }
            Misframe::Apple
        } else if cropped {
            Misframe::Cropped
        } else {
            match library_turn(&f) {
                0 => continue,
                q => Misframe::Turned(q),
            }
        };
        let turn = match kind {
            Misframe::Turned(q) => Some(q),
            _ => None,
        };
        if kind == Misframe::Apple && apple_boxes.is_none() {
            let library = f.path.ancestors().find(|a| a.extension().is_some_and(|e| e == "photoslibrary")).map(Path::to_path_buf);
            apple_boxes = Some(library.and_then(|l| crate::apple_photos::read_faces(&l).ok()).unwrap_or_default().into_iter().map(|g| (g.id, g.rect_list)).collect());
        }
        let now = since.max(crate::now());
        let tx = db.conn.unchecked_transaction().map_err(fe)?;
        let faces: Vec<(i64, f64, f64, f64, f64, Option<Vec<u8>>, bool, Option<String>)> = {
            let mut st = tx.prepare("SELECT id, x, y, w, h, landmarks, local, apple_id FROM face WHERE image=?1 AND source='graineo'").map_err(fe)?;
            let r = st
                .query_map([image], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get::<_, i64>(6)? != 0, r.get(7)?)))
                .map_err(fe)?
                .collect::<rusqlite::Result<_>>()
                .map_err(fe)?;
            r
        };
        // their face images showed the old place (and a new face may get the number of a removed one)
        let touched: Vec<PathBuf> = faces.iter().map(|f| crate::face_index::thumb_file(f.0, image, f.1, f.2, f.3, f.4)).collect();
        for (id, x, y, b, h, points, local, apple_id) in faces {
            let apple_box = apple_id.as_ref().filter(|_| kind == Misframe::Apple).and_then(|a| apple_boxes.as_ref()?.get(a).copied());
            match (turn, apple_box) {
                (None, Some((ax, ay, ab, ah))) => {
                    tx.execute("UPDATE face SET x=?2, y=?3, w=?4, h=?5, landmarks=NULL, detected=?6 WHERE id=?1", params![id, ax, ay, ab, ah, now]).map_err(fe)?;
                    report.from_apple += 1;
                }
                _ if !local && apple_id.is_none() => {
                    tx.execute("DELETE FROM face WHERE id=?1", [id]).map_err(fe)?;
                    report.removed += 1;
                }
                (Some(q), _) => {
                    let (nx, ny, nb, nh) = turn_box(x, y, b, h, q);
                    let points = points.map(|p| {
                        let v = crate::people::read_blob(&p);
                        let mut out = Vec::with_capacity(v.len());
                        for c in v.chunks_exact(2) {
                            let (px, py) = turn_point(c[0], c[1], q);
                            out.extend([px, py]);
                        }
                        crate::people::blob(&out)
                    });
                    tx.execute("UPDATE face SET x=?2, y=?3, w=?4, h=?5, landmarks=?6, detected=?7 WHERE id=?1", params![id, nx, ny, nb, nh, points, now]).map_err(fe)?;
                    report.turned += 1;
                }
                (None, None) => {
                    tx.execute("UPDATE face SET detected=?2 WHERE id=?1", params![id, now]).map_err(fe)?;
                    report.kept += 1;
                }
            }
        }
        tx.execute("UPDATE image SET faces_scanned=0 WHERE id=?1", [image]).map_err(fe)?;
        tx.commit().map_err(fe)?;
        for p in touched {
            let _ = std::fs::remove_file(p);
        }
        report.images += 1;
    }
    db.set_info("face_frames_open", &serde_json::to_string(&later).unwrap_or_default()).map_err(|e| e.to_string())?;
    db.set_info("face_frames", REVISION).map_err(|e| e.to_string())?;
    Ok(Some(report))
}

/// Box (normalized, origin top left) of an image turned by `quarter` clockwise quarter turns.
pub fn turn_box(x: f64, y: f64, b: f64, h: f64, quarter: u8) -> (f64, f64, f64, f64) {
    match quarter % 4 {
        1 => (1.0 - y - h, x, h, b),
        2 => (1.0 - x - b, 1.0 - y - h, b, h),
        3 => (y, 1.0 - x - b, h, b),
        _ => (x, y, b, h),
    }
}

/// Point (normalized) of an image turned by `quarter` clockwise quarter turns.
pub fn turn_point(x: f32, y: f32, quarter: u8) -> (f32, f32) {
    match quarter % 4 {
        1 => (1.0 - y, x),
        2 => (1.0 - x, 1.0 - y),
        3 => (y, 1.0 - x),
        _ => (x, y),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A box turned with the image lands on the same pixels (checked against `image::imageops::rotate90`).
    #[test]
    fn turned_box_follows_the_pixels() {
        let mut img = RgbImage::new(40, 20);
        // red block at x 4..12, y 2..8
        for y in 2..8 {
            for x in 4..12 {
                img.put_pixel(x, y, image::Rgb([255, 0, 0]));
            }
        }
        let (x, y, b, h) = (4.0 / 40.0, 2.0 / 20.0, 8.0 / 40.0, 6.0 / 20.0);
        for q in 0..4u8 {
            let t = crate::preview::rotate(img.clone(), q);
            let (tx, ty, tb, th) = turn_box(x, y, b, h, q);
            let (w, hh) = (t.width() as f64, t.height() as f64);
            let (cx, cy) = (((tx + tb / 2.0) * w) as u32, ((ty + th / 2.0) * hh) as u32);
            assert_eq!(t.get_pixel(cx, cy).0, [255, 0, 0], "quarter {q}");
            assert!(((tb * w) - if q % 2 == 1 { 6.0 } else { 8.0 }).abs() < 1e-9);
            let (px, py) = turn_point(x as f32 + 0.01, y as f32 + 0.01, q);
            let p = t.get_pixel(((px as f64) * w) as u32, ((py as f64) * hh) as u32);
            assert_eq!(p.0, [255, 0, 0], "point, quarter {q}");
        }
    }

    /// A photo cropped in Apple Photos (known from the sync): grid previews and the single view get Apple's current
    /// version – the full size render if on this Mac, else Photos' preview; other photos keep their original.
    #[cfg(target_os = "macos")]
    #[test]
    fn photo_cropped_in_apple_photos_shows_the_current_version() {
        let lib = std::env::temp_dir().join(format!("graineo-reshaped-{}.photoslibrary", std::process::id()));
        let (cropped, plain) = ("B1B2C3D4-0000-4000-8000-000000000002", "B1B2C3D4-0000-4000-8000-000000000003");
        std::fs::create_dir_all(lib.join("originals/B")).unwrap();
        std::fs::create_dir_all(lib.join("resources/derivatives/B")).unwrap();
        std::fs::create_dir_all(lib.join("resources/renders/B")).unwrap();
        for u in [cropped, plain] {
            RgbImage::from_pixel(200, 400, image::Rgb([0, 0, 200])).save(lib.join(format!("originals/B/{u}.png"))).unwrap();
            RgbImage::from_pixel(100, 100, image::Rgb([200, 0, 0])).save_with_format(lib.join(format!("resources/derivatives/B/{u}_1_105_c.jpeg")), image::ImageFormat::Jpeg).unwrap();
        }
        crate::apple_photos::set_reshaped([cropped.to_string()].into_iter().collect());
        let path = |u: &str| lib.join(format!("originals/B/{u}.png"));
        // Photos' preview (no render on this Mac)
        let (img, _) = crate::preview::load(&path(cropped), "PNG", 512).unwrap();
        assert_eq!((img.width(), img.height()), (100, 100));
        assert!(img.get_pixel(50, 50).0[0] > 150);
        // not cropped: the original
        let (img, _) = crate::preview::load(&path(plain), "PNG", 512).unwrap();
        assert_eq!((img.width(), img.height()), (200, 400));
        assert_eq!(crate::apple_photos::current_version(&path(plain)), None);
        // the full size render wins over the preview
        RgbImage::from_pixel(300, 300, image::Rgb([0, 200, 0])).save_with_format(lib.join(format!("resources/renders/B/{cropped}_1_201_a.jpeg")), image::ImageFormat::Jpeg).unwrap();
        let (img, _) = crate::preview::load(&path(cropped), "PNG", 512).unwrap();
        assert_eq!((img.width(), img.height()), (300, 300));
        crate::apple_photos::set_reshaped(Default::default());
        let _ = std::fs::remove_dir_all(lib);
    }

    #[test]
    fn box_in_photo_needs_half_of_its_area_inside() {
        assert!(box_in_photo(0.4, 0.3, 0.1, 0.1));
        assert!(box_in_photo(0.95, 0.3, 0.1, 0.1));
        assert!(!box_in_photo(0.96, 0.3, 0.1, 0.1));
        assert!(!box_in_photo(0.2, -0.2, 0.1, 0.1));
        assert!(!box_in_photo(0.2, 0.2, 0.0, 0.1));
    }

    #[test]
    fn upright_size_swaps_for_turned_photos() {
        assert_eq!(upright_size(Some(640), Some(480), Some(6)), Some((480, 640)));
        assert_eq!(upright_size(Some(640), Some(480), Some(3)), Some((640, 480)));
        assert_eq!(upright_size(None, Some(480), None), None);
    }
}
