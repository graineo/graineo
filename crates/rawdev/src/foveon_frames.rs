//! Foveon (X3F) frames of a merge. Every exposure goes through the Foveon development up to the linear layers
//! (`develop::Session` + `develop::linearize`: defect repair, black level, spatial gain and colour shading of the
//! camera – nothing of the single-photo development changes); the three layers are the colour of the merge. The layers
//! are scaled per layer so that 1.0 is the saturation count in the middle of the frame (the spatial gain stays in the
//! values: the flat-field correction is part of the result); the clip level of the bracket comes from the frames
//! themselves ([`hdr::frames::planar_clip`]).
//!
//! The merged photo is developed by the general path: the colour of the reference's camera calibration (camera → XYZ
//! of its colour mode "Standard", as-shot white balance) is handed over as a DNG-style camera matrix.

use std::path::Path;

/// Is `path` a single Foveon frame (X3F; an SFD file X3I holds a bracket itself and merges on its own)?
pub fn is_foveon(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("x3f"))
}

/// What the merge and the document need of one X3F: camera data and its linear layers.
pub struct Layers {
    pub info: raw::Info,
    pub w: usize,
    pub h: usize,
    /// linear layer values, 1.0 = saturation count (middle of the frame)
    pub px: Vec<[f32; 3]>,
}

fn unsupported(path: &Path, why: String) -> hdr::Error {
    hdr::Error::Read { path: path.to_path_buf(), why }
}

/// EXIF of the embedded camera JPEG (exposure, lens, date …).
fn exif_of(path: &Path) -> Option<x3f::exif::JpegExif> {
    [true, false].into_iter().find_map(|large| x3f::exif::parse_jpeg_exif(&x3f::read_embedded_jpeg_up_to(path, large, 128 * 1024).ok()?))
}

/// EXIF orientation of the camera's quarter turns (clockwise to straighten).
fn orientation(quarters: u8) -> u16 {
    match quarters % 4 {
        1 => 6,
        2 => 3,
        3 => 8,
        _ => 1,
    }
}

/// Camera → XYZ (D65) of the reference, inverted as a DNG ColorMatrix (XYZ → camera), and the as-shot white balance
/// as multipliers normalised to the middle layer.
fn colour(cal: &develop::Calibration) -> Option<([[f32; 3]; 3], [f32; 3])> {
    let xyz_to_cam = develop::camera_to_xyz(cal, None).inverse()?.to_f32();
    let m: [[f32; 3]; 3] = std::array::from_fn(|r| std::array::from_fn(|c| xyz_to_cam[r * 3 + c]));
    let g = cal.wb_gain;
    let wb = [(g[0] / g[1]) as f32, 1.0, (g[2] / g[1]) as f32];
    (m.iter().flatten().all(|v| v.is_finite()) && wb.iter().all(|v| v.is_finite() && *v > 0.0)).then_some((m, wb))
}

/// Camera data of an X3F for the general path (sensor `Linear`, three channels, the camera's colour as matrix).
pub fn info(path: &Path, cal: &develop::Calibration, w: usize, h: usize) -> Result<raw::Info, hdr::Error> {
    let (matrix, wb) = colour(cal).ok_or_else(|| unsupported(path, "Foveon colour calibration not usable".into()))?;
    let e = exif_of(path);
    let e = e.as_ref();
    Ok(raw::Info {
        format: "X3F".into(),
        maker: "SIGMA".into(),
        model: e.and_then(|e| e.model.clone()).map(|m| m.trim_start_matches("SIGMA ").to_string()).unwrap_or_else(|| "Foveon".into()),
        mode: String::new(),
        width: w,
        height: h,
        channels: 3,
        bits: 16,
        sensor: raw::Sensor::Linear,
        cfa: String::new(),
        cfa_size: (1, 1),
        black: vec![0.0],
        black_pattern: (1, 1, 1),
        white: vec![1.0],
        wb: Some(wb),
        matrix: Some(("D65".into(), matrix)),
        matrix_illuminants: vec!["D65".into()],
        orientation: orientation(cal.orientation),
        active: None,
        crop: None,
        fuji_rotated: false,
        baseline_exposure: 0.0,
        dng_profile: Vec::new(),
        exif: raw::Exif {
            iso: e.and_then(|e| e.iso).or_else(|| (cal.iso > 0.0).then_some(cal.iso.round() as u32)),
            exposure_time: e.and_then(|e| e.exposure_time).map(|v| v as f32),
            aperture: e.and_then(|e| e.f_number).map(|v| v as f32),
            focal_length: e.and_then(|e| e.focal_length).map(|v| v as f32),
            lens: e.and_then(|e| e.lens_model.clone()),
            capture_date: e.and_then(|e| e.date_time.clone()),
            serial_number: e.and_then(|e| e.serial.clone()),
            ..Default::default()
        },
        corrections: Vec::new(),
        calibration: vec![raw::color::Calibration { kelvin: 6504.0, color: matrix, forward: None }],
        analog_balance: None,
        fuji_dr: None,
    })
}

/// Saturation count per layer after the linearisation in the middle of the frame (spatial gain and colour shading
/// make it differ over the sensor): the linearisation of a small raw image that holds the saturation count everywhere.
fn saturation(s: &develop::Session) -> [f32; 3] {
    let raw = s.raw();
    let planes: [Vec<u16>; 3] = std::array::from_fn(|c| vec![(s.cal.black[c] + s.cal.saturation).round().clamp(0.0, 65535.0) as u16; raw.columns * raw.rows]);
    let full = x3f::RawImage { columns: raw.columns, rows: raw.rows, planes };
    let lin = develop::linearize_scaled(&full, &s.cal, 8);
    let (w, h) = (lin.width, lin.height);
    let i = (h / 2 * w + w / 2) * 3;
    std::array::from_fn(|c| lin.data.get(i + c).copied().filter(|v| v.is_finite() && *v > 0.0).unwrap_or(1.0))
}

/// Raw count per layer at which the layer clips in this frame (`None`: it does not clip): the layer's largest stored
/// count over the image area, when at least 0.05 % of the pixels pile up within 1 % below it. Measured on the stored
/// counts, before the spatial gain spreads a clipped area over a range of values.
fn raw_clip(raw: &x3f::RawImage, active: [usize; 4]) -> [Option<u16>; 3] {
    use rayon::prelude::*;
    let [x0, y0, x1, y1] = active;
    let rows: Vec<usize> = (y0..=y1.min(raw.rows.saturating_sub(1))).collect();
    let x1 = x1.min(raw.columns.saturating_sub(1));
    std::array::from_fn(|c| {
        let plane = &raw.planes[c];
        let max = rows.par_iter().map(|&y| plane[y * raw.columns + x0..=y * raw.columns + x1].iter().copied().max().unwrap_or(0)).max().unwrap_or(0);
        let near = (max as f32 * 0.99) as u16;
        let n: usize = rows.par_iter().map(|&y| plane[y * raw.columns + x0..=y * raw.columns + x1].iter().filter(|v| **v >= near).count()).sum();
        let total = rows.len() * (x1 + 1 - x0.min(x1 + 1));
        (max > 0 && n * 2000 >= total).then_some(max)
    })
}

/// Read one X3F frame: its linear layers (1.0 = saturation count in the middle of the frame) and camera data. Pixels
/// with a clipped layer are grey at their brightest white-balanced value (a saturated Foveon layer leaves the others
/// at a wrong ratio: yellow-green in a blown sky) at or above the returned level per layer, from which the merge counts
/// a value as clipped (`f32::MAX`: nothing clipped).
pub fn load(path: &Path) -> Result<(Layers, [f32; 3]), hdr::Error> {
    let s = develop::Session::open(path).map_err(|e| unsupported(path, e.to_string()))?;
    let sat = saturation(&s);
    let lin = develop::linearize(s.raw(), &s.cal);
    let (w, h) = (lin.width, lin.height);
    let info = info(path, &s.cal, w, h)?;
    let wb = info.wb.unwrap_or([1.0; 3]);
    let k = sat.map(|v| 1.0 / v);
    let mut px: Vec<[f32; 3]> = lin.data.chunks_exact(3).map(|c| [c[0] * k[0], c[1] * k[1], c[2] * k[2]]).collect();
    drop(lin);
    let rc = raw_clip(s.raw(), s.cal.active);
    let level = if rc.iter().any(Option::is_some) {
        let raw = s.raw();
        let [x0, y0, ..] = s.cal.active;
        let limit: [u16; 3] = std::array::from_fn(|c| rc[c].map_or(u16::MAX, |m| (m as f32 * 0.97) as u16));
        let clipped = |x: usize, y: usize| {
            let (sx, sy) = (x0 + x, y0 + y);
            sx < raw.columns && sy < raw.rows && {
                let p = raw.pixel(sy, sx);
                (0..3).any(|c| p[c] >= limit[c])
            }
        };
        // the level: the 99.9 % value of the pixels that do not clip (a value above it counts as clipped – a few
        // bright unclipped pixels then come from a shorter frame, which costs a little noise and nothing else)
        let level: [f32; 3] = std::array::from_fn(|c| {
            let mut v: Vec<f32> = px.iter().enumerate().step_by(7).filter(|(i, _)| !clipped(i % w, i / w)).map(|(_, p)| p[c]).collect();
            v.sort_by(|a, b| a.total_cmp(b));
            v.get(((v.len().max(1) - 1) as f64 * 0.999) as usize).copied().unwrap_or(1.0).max(1e-3)
        });
        neutralize(&mut px, w, &clipped, wb, level);
        level
    } else {
        [f32::MAX; 3]
    };
    drop(s);
    Ok((Layers { info, w, h, px }, level))
}

/// Grey the pixels `clipped(x, y)` names at their brightest white-balanced value, lifted (still grey) until every
/// layer is at least `level`: they then count as clipped in the merge.
pub fn neutralize(px: &mut [[f32; 3]], w: usize, clipped: &(dyn Fn(usize, usize) -> bool + Sync), wb: [f32; 3], level: [f32; 3]) {
    use rayon::prelude::*;
    px.par_chunks_mut(w.max(1)).enumerate().for_each(|(y, row)| {
        for (x, p) in row.iter_mut().enumerate() {
            if clipped(x, y) {
                let l = (0..3).map(|c| p[c] * wb[c]).fold(1e-6f32, f32::max);
                let g = [l / wb[0], l / wb[1], l / wb[2]];
                let k = (0..3).map(|c| level[c] / g[c]).fold(1f32, f32::max);
                *p = g.map(|v| v * k);
            }
        }
    });
}

/// One frame for the merge core; its `clip` is the frame's own level ([`load`]) until [`share_clip`] sets the
/// bracket's.
pub fn frame(path: &Path) -> Result<hdr::frames::Frame, hdr::Error> {
    let (l, level) = load(path)?;
    Ok(hdr::frames::from_planar(path.to_path_buf(), l.info, l.px, l.w, l.h, level))
}

/// The clip level of a Foveon bracket: per layer the lowest level of its frames; without any clipped pixel the measured pile-up of [`hdr::frames::planar_clip`].
pub fn bracket_clip(levels: &[[f32; 3]], planes: &[&[[f32; 3]]]) -> [f32; 3] {
    let low: [f32; 3] = std::array::from_fn(|c| levels.iter().map(|l| l[c]).fold(f32::MAX, f32::min));
    let fallback = hdr::frames::planar_clip(planes);
    std::array::from_fn(|c| if low[c] < f32::MAX { 0.999 * low[c] } else { fallback[c].max(1.5) })
}

/// Give all Foveon frames of a bracket the clip level of the whole bracket ([`bracket_clip`]) and rebuild their
/// proxies. Mosaic frames stay as they are.
pub fn share_clip(frames: &mut [hdr::frames::Frame]) {
    let levels: Vec<[f32; 3]> = frames.iter().filter(|f| f.planar.is_some()).map(|f| f.clip).collect();
    if levels.is_empty() {
        return;
    }
    let planes: Vec<&[[f32; 3]]> = frames.iter().filter_map(|f| f.planar.as_deref()).collect();
    let clip = bracket_clip(&levels, &planes);
    for f in frames.iter_mut() {
        if f.planar.is_some() && f.clip != clip {
            if std::env::var_os("GRAINEO_LOG").is_some() {
                eprintln!("foveon frame {}: own clip level {:?}, bracket {clip:?}", f.path.display(), f.clip);
            }
            let px = f.planar.take().unwrap_or_default();
            *f = hdr::frames::from_planar(f.path.clone(), f.info.clone(), px, f.w, f.h, clip);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_x3f_counts_as_a_foveon_frame() {
        assert!(is_foveon(Path::new("/a/DP2M5418.X3F")) && is_foveon(Path::new("/a/b.x3f")));
        assert!(!is_foveon(Path::new("/a/SDQH1646.X3I")) && !is_foveon(Path::new("/a/IMG_1.CR2")));
    }

    #[test]
    fn quarter_turns_map_to_exif_orientation() {
        assert_eq!([0, 1, 2, 3].map(orientation), [1, 6, 3, 8]);
        // and back the way the documents read it
        assert_eq!([1, 6, 3, 8].map(crate::doc::quarters), [0, 1, 2, 3]);
    }

    #[test]
    fn shared_clip_is_the_lowest_level_of_the_frames() {
        let (w, h) = (8, 8);
        let info = || blank();
        let mut fs = vec![
            hdr::frames::from_planar("/a".into(), info(), vec![[0.1f32; 3]; w * h], w, h, [f32::MAX; 3]),
            hdr::frames::from_planar("/b".into(), info(), vec![[0.5f32, 0.6, 0.7]; w * h], w, h, [0.8, 0.9, 0.95]),
            hdr::frames::from_planar("/c".into(), info(), vec![[0.5f32, 0.6, 0.7]; w * h], w, h, [0.85, 0.88, 1.2]),
        ];
        share_clip(&mut fs);
        let want = [0.999 * 0.8, 0.999 * 0.88, 0.999 * 0.95];
        for f in &fs {
            assert!((0..3).all(|c| (f.clip[c] - want[c]).abs() < 1e-6), "{:?}", f.clip);
            assert!(f.planar.as_ref().is_some_and(|p| p.len() == w * h));
        }
    }

    #[test]
    fn clipped_pixels_turn_grey_at_their_brightest_value() {
        let wb = [2.0f32, 1.0, 1.5];
        let mut px = vec![[0.2f32, 0.3, 0.1], [0.4, 0.95, 0.5], [0.1, 0.2, 0.1]];
        neutralize(&mut px, 3, &|x, _| x >= 1, wb, [0.3, 0.6, 0.4]);
        assert_eq!(px[0], [0.2, 0.3, 0.1], "not clipped: unchanged");
        // brightest white-balanced value 0.95 (middle layer): grey after the balance, above the level already
        let p = px[1];
        assert!((p[0] * wb[0] - 0.95).abs() < 1e-6 && (p[1] - 0.95).abs() < 1e-6 && (p[2] * wb[2] - 0.95).abs() < 1e-6);
        // a dim clipped pixel: grey, lifted until every layer reaches the level
        let q = px[2];
        assert!((q[0] * wb[0] - q[1]).abs() < 1e-5 && (q[2] * wb[2] - q[1]).abs() < 1e-5, "{q:?}");
        assert!(q[0] >= 0.3 - 1e-6 && q[1] >= 0.6 - 1e-6 && q[2] >= 0.4 - 1e-6, "{q:?}");
    }

    #[test]
    fn a_pile_up_of_raw_counts_is_a_clipped_layer() {
        let (columns, rows) = (200, 100);
        // layer 0: a blown area at 4000 (10 % of the pixels); layer 1: a single bright pixel, no pile; layer 2: empty
        let planes: [Vec<u16>; 3] = [
            (0..columns * rows).map(|i| if i % 10 == 0 { 4000 } else { 1000 }).collect(),
            (0..columns * rows).map(|i| if i == 7 { 3000 } else { 500 + (i % 50) as u16 }).collect(),
            vec![0; columns * rows],
        ];
        let raw = x3f::RawImage { columns, rows, planes };
        assert_eq!(raw_clip(&raw, [0, 0, columns - 1, rows - 1]), [Some(4000), None, None]);
    }

    pub(super) fn blank() -> raw::Info {
        raw::Info {
            format: "X3F".into(),
            maker: "SIGMA".into(),
            model: "test".into(),
            mode: String::new(),
            width: 8,
            height: 8,
            channels: 3,
            bits: 16,
            sensor: raw::Sensor::Linear,
            cfa: String::new(),
            cfa_size: (1, 1),
            black: vec![0.0],
            black_pattern: (1, 1, 1),
            white: vec![1.0],
            wb: Some([1.0; 3]),
            matrix: None,
            matrix_illuminants: Vec::new(),
            orientation: 1,
            active: None,
            crop: None,
            fuji_rotated: false,
            baseline_exposure: 0.0,
            dng_profile: Vec::new(),
            exif: Default::default(),
            corrections: Vec::new(),
            calibration: Vec::new(),
            analog_balance: None,
            fuji_dr: None,
        }
    }
}
