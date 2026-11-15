//! Camera internals as data: white balance (as shot, daylight from the camera matrix),
//! black/white point, ISO, Fuji DR mode, picture style (makernote), sensor type and pattern, bit depth.

use serde_json::{json, Value};

use crate::{region_json, pattern_text, round_to, Layer, Options, RawImage, Result};

/// White points (XYZ, Y = 1) of the matrix illuminants.
fn white_xyz(name: &str) -> Option<[f32; 3]> {
    match name {
        "D65" => Some([0.95047, 1.0, 1.08883]),
        "D55" | "Daylight" | "FineWeather" => Some([0.95682, 1.0, 0.92149]),
        "D50" => Some([0.96422, 1.0, 0.82521]),
        "D75" => Some([0.94972, 1.0, 1.22638]),
        _ => None,
    }
}

pub(crate) fn analyze(rb: &RawImage, _o: &Options) -> Result<(Vec<Layer>, Value)> {
    let per = rb.period;
    let z = rb.crop;
    let mut kz = json!({
        "sensor": {
            "type": if per == 2 { "Bayer" } else { "X-Trans" },
            "period": per,
            "pattern_sensor": pattern_text(&rb.pattern, 0, 0, per),
            "pattern_image": pattern_text(&rb.pattern, z.x, z.y, per),
            "size": [rb.width, rb.height],
            "active": region_json(rb.active),
            "crop": region_json(z),
            "black_borders": rb.black_margins.iter().map(|b| region_json(*b)).collect::<Vec<_>>(),
        },
        "white_dn": rb.white_dn,
        "black_dn_rgb": rb.black_dn.map(|v| round_to(v as f64, 1)),
        "wb_shot": rb.wb.map(|v| round_to(v as f64, 4)),
        "standard_ev": round_to(rb.standard_ev() as f64, 2),
        "decode_time_ms": round_to(rb.decode_time.as_secs_f64() * 1000.0, 0),
    });
    let Some(i) = &rb.info else { return Ok((Vec::new(), kz)) };
    // Daylight WB: the camera's neutral under the matrix illuminant (XYZ→camera · white point)
    let daylight = i.matrix.as_ref().and_then(|(name, m)| {
        let w = white_xyz(name)?;
        let n: [f32; 3] = std::array::from_fn(|r| m[r][0] * w[0] + m[r][1] * w[1] + m[r][2] * w[2]);
        if n.iter().any(|v| *v <= 0.0) {
            return None;
        }
        let mul = [n[1] / n[0], 1.0, n[1] / n[2]];
        Some(json!({"illuminant": name, "multipliers": mul.map(|v| round_to(v as f64, 4))}))
    });
    let fuji_dr = i.fuji_dr.map(|d| {
        json!({"setting": format!("DR{d}"), "underexposed_ev": round_to(raw::development::fuji_dr_ev(Some(d)) as f64, 2),
               "hint": "The camera exposes the RAW this many stops tighter and lifts it again in the JPEG – headroom for highlights"})
    });
    let style = rb.file.as_deref().and_then(raw::picture_style::read).map(|b| {
        json!({"style": b.style, "details": b.details.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>()})
    });
    let e = &i.exif;
    let m = kz.as_object_mut().unwrap();
    m.insert("maker".into(), json!(i.maker));
    m.insert("model".into(), json!(i.model));
    m.insert("format".into(), json!(i.format));
    m.insert("mode".into(), json!(i.mode));
    m.insert("bit_depth".into(), json!(i.bits));
    m.insert("cfa_rawler".into(), json!(i.cfa));
    m.insert("black_pattern".into(), json!({"values": i.black, "size": [i.black_pattern.0, i.black_pattern.1, i.black_pattern.2]}));
    m.insert("white_channels_dn".into(), json!(i.white));
    m.insert("wb_daylight".into(), daylight.unwrap_or(Value::Null));
    m.insert("matrix".into(), json!(i.matrix.as_ref().map(|(n, mm)| json!({"illuminant": n, "xyz_to_camera": mm}))));
    m.insert("matrix_illuminants".into(), json!(i.matrix_illuminants));
    m.insert("analog_balance".into(), json!(i.analog_balance));
    m.insert("fuji_dr".into(), fuji_dr.unwrap_or(Value::Null));
    m.insert("picture_style".into(), style.unwrap_or(Value::Null));
    m.insert("baseline_exposure_ev".into(), json!(i.baseline_exposure));
    m.insert("camera_ev".into(), json!(raw::development::camera_ev(&i.maker, &i.model)));
    m.insert("dng_profile".into(), json!(i.dng_profile));
    m.insert("orientation".into(), json!(i.orientation));
    m.insert("corrections".into(), json!(i.corrections));
    m.insert(
        "shot".into(),
        json!({"iso": e.iso, "time_s": e.exposure_time, "aperture": e.aperture, "focal_length_mm": e.focal_length, "distance_m": e.distance,
               "lens": e.lens, "lens_maker": e.lens_maker, "lens_rawler": e.lens_db, "datum": e.capture_date}),
    );
    Ok((Vec::new(), kz))
}
