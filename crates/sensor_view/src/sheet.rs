//! Demo sheet: all analyses of one image on a single page (test output, `graineo-dev sensor --sheet`).
//! Also shows how the layers are placed over the preview (reference area → preview pixels).

use serde_json::Value;

use crate::canvas::{self, Canvas};
use crate::{Kind, Layer, Outcome, Options, RawImage, Role};

const TILE: usize = 760;
const MARGIN: usize = 16;

/// Place a layer with a reference area (image coordinates) over an image of size `target` (covering the crop).
pub fn overlay(target: &mut Canvas, layer: &Layer, rb: &RawImage, opacity: f32) {
    let z = rb.crop;
    let Some(b) = layer.reference else { return };
    let fx = target.width as f32 / z.w as f32;
    let fy = target.height as f32 / z.h as f32;
    let (x, y) = ((b.x as f32 * fx).round() as isize, (b.y as f32 * fy).round() as isize);
    let (w, h) = (((b.w as f32 * fx).round() as usize).max(1), ((b.h as f32 * fy).round() as usize).max(1));
    let mut e = layer.canvas.scaled(w, h);
    if opacity < 1.0 {
        for p in e.rgba.chunks_exact_mut(4) {
            p[3] = (p[3] as f32 * opacity) as u8;
        }
    }
    target.insert_at(&e, x, y);
}

struct Tile {
    title: String,
    canvas: Canvas,
}

fn tile(title: impl Into<String>, b: &Canvas) -> Tile {
    let f = (TILE as f32 / b.width as f32).min(if b.width < TILE / 2 { 4.0 } else { 1.0 });
    let (w, h) = (((b.width as f32 * f) as usize).max(1), ((b.height as f32 * f) as usize).max(1));
    let canvas = if f < 1.0 { b.downscaled(w, h) } else { b.scaled(w, h) };
    Tile { title: title.into(), canvas }
}

fn number(v: &Value, path: &[&str]) -> String {
    let mut x = v;
    for p in path {
        x = match p.parse::<usize>() {
            Ok(i) => &x[i],
            Err(_) => &x[*p],
        };
    }
    match x {
        Value::Null => "–".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => format!("{}", n.as_f64().map(|f| (f * 100.0).round() / 100.0).unwrap_or(0.0)),
        o => o.to_string(),
    }
}

/// Assemble the sheet from the results.
pub fn demo_sheet(rb: &RawImage, outcomes: &[Outcome], o: &Options) -> Canvas {
    let basis = rb.preview(o.size.min(1024));
    let results = |a: Kind| outcomes.iter().find(|e| e.kind == a);
    let mut tiles: Vec<Tile> = Vec::new();
    let mut rows_text: Vec<String> = Vec::new();

    if let Some(e) = results(Kind::Internals) {
        let k = &e.metrics;
        rows_text.push(format!("{} {} · {} · {} · {} Bit", number(k, &["maker"]), number(k, &["model"]), number(k, &["format"]), number(k, &["sensor", "type"]), number(k, &["bit_depth"])));
        rows_text.push(format!(
            "ISO {} · 1/{:.0} s · f/{} · {} mm",
            number(k, &["shot", "iso"]),
            1.0 / k["shot"]["time_s"].as_f64().unwrap_or(1.0),
            number(k, &["shot", "aperture"]),
            number(k, &["shot", "focal_length_mm"])
        ));
        rows_text.push(format!("Pattern (image): {}", k["sensor"]["pattern_image"].as_array().map(|a| a.iter().map(|s| s.as_str().unwrap_or("")).collect::<Vec<_>>().join("/")).unwrap_or_default()));
        rows_text.push(format!("Black {} · White {} DN · WB {} · Daylight {}", number(k, &["black_dn_rgb"]), number(k, &["white_dn"]), number(k, &["wb_shot"]), number(k, &["wb_daylight", "multipliers"])));
        rows_text.push(format!("Fuji DR: {} · Picture style: {}", number(k, &["fuji_dr", "setting"]), number(k, &["picture_style", "style"])));
    }
    if let Some(e) = results(Kind::Histogram) {
        let k = &e.metrics;
        rows_text.push(format!("Headroom {} EV (RGB {}) · clipped {} %", number(k, &["headroom_ev"]), number(k, &["headroom_ev_rgb"]), number(k, &["clipped_percent"])));
        let j = &k["render_compare"]["camera_jpeg"];
        if j.is_object() && j.get("error").is_none() {
            rows_text.push(format!(
                "JPEG blown out {} % – of which RAW clipped {} %, headroom ≥ 0.5 EV {} % (median {} EV)",
                number(j, &["blown_percent"]),
                number(j, &["of_which_raw_clipped_percent"]),
                number(j, &["of_which_with_reserve_from_0_5ev_percent"]),
                number(j, &["reserve_median_ev"])
            ));
        }
        let mut b = basis.clone();
        if let Some(eb) = e.layer("clipping") {
            overlay(&mut b, eb, rb, 1.0);
        }
        tiles.push(tile("Clipping (R/G/B, white = all)", &b));
        if let Some(eb) = e.layer("jpeg_reserve") {
            let mut b = basis.clone();
            overlay(&mut b, eb, rb, 1.0);
            tiles.push(tile("JPEG blown out: green headroom ≥ ½ EV, yellow < ½, red RAW clipped", &b));
        }
        for n in ["histogram_ev", "histogram_linear"] {
            if let Some(eb) = e.layer(n) {
                tiles.push(tile(n, &eb.canvas));
            }
        }
    }
    if let Some(e) = results(Kind::Mosaic) {
        let k = &e.metrics;
        if let Some(eb) = e.layer("mosaic") {
            tiles.push(tile(format!("Mosaic {} %, detail {} (pattern {})", number(k, &["zoom_percent"]), number(k, &["crop"]), number(k, &["pattern_crop", "0"])), &eb.canvas));
        }
    }
    if let Some(e) = results(Kind::DemosaicComparison) {
        let k = &e.metrics;
        for eb in e.layers.iter().filter(|e| e.role == Role::View) {
            tiles.push(tile(format!("{} – false color Ø {}", eb.name, number(k, &["method", &eb.name, "mean"])), &eb.canvas));
        }
        if let (Some(r), Some(f)) = (e.layers.iter().find(|e| e.role == Role::View), e.layer("false_color")) {
            let mut b = r.canvas.clone();
            for p in b.rgba.chunks_exact_mut(4) {
                for c in 0..3 {
                    p[c] /= 3;
                }
            }
            b.overlay(&f.canvas, 1.0);
            tiles.push(tile(format!("False color warning ({})", number(k, &["reference"])), &b));
        }
        if let Some(eb) = e.layer("deviation") {
            let mut b = Canvas::new_at(eb.canvas.width, eb.canvas.height, canvas::BLACK);
            b.overlay(&eb.canvas, 1.0);
            tiles.push(tile(format!("Deviation {} ↔ {}", number(k, &["deviation", "between", "0"]), number(k, &["deviation", "between", "1"])), &b));
        }
        if let Some(eb) = e.layer("false_color_map") {
            let mut b = basis.clone();
            overlay(&mut b, eb, rb, 1.0);
            // mark the detail area
            if let Some(a) = e.layers.first().and_then(|e| e.reference) {
                let f = b.width as f32 / rb.crop.w as f32;
                b.frame((a.x as f32 * f) as isize, (a.y as f32 * f) as isize, ((a.w as f32 * f) as usize).max(3), ((a.h as f32 * f) as usize).max(3), [0, 255, 255, 255]);
            }
            tiles.push(tile(format!("Moiré map whole image ({} ms), turquoise = detail", number(k, &["whole_image", "duration_ms"])), &b));
        }
        rows_text.push(format!("False color: warning blocks {} % · G1/G2 {} %", number(k, &["whole_image", "warning_blocks_percent"]), number(k, &["g1_g2_imbalance_percent"])));
    }
    if let Some(e) = results(Kind::Noise) {
        let k = &e.metrics;
        let mut b = basis.clone();
        for p in b.rgba.chunks_exact_mut(4) {
            for c in 0..3 {
                p[c] /= 2;
            }
        }
        if let Some(eb) = e.layer("noise_g") {
            overlay(&mut b, eb, rb, 1.0);
        }
        tiles.push(tile("SNR G in flat blocks (0–45 dB)", &b));
        let mut b = basis.clone();
        if let Some(eb) = e.layer("noise_ratio") {
            overlay(&mut b, eb, rb, 1.0);
        }
        tiles.push(tile("Noise measured / model (blue < 1 < red)", &b));
        if let Some(eb) = e.layer("snr_curve") {
            tiles.push(tile("SNR curve", &eb.canvas));
        }
        rows_text.push(format!(
            "Usable DR (G): {} EV ({}) · Model: {} EV to noise floor, {} EV at SNR 1, {} EV at SNR 10 · Black border: {} EV · Scene {} EV",
            number(k, &["dynamic_range_usable", "ev"]),
            number(k, &["dynamic_range_usable", "source"]),
            number(k, &["dynamic_range_model", "dr_ev_noise_floor", "1"]),
            number(k, &["dynamic_range_model", "dr_ev_snr1", "1"]),
            number(k, &["dynamic_range_model", "dr_ev_snr10", "1"]),
            number(k, &["dynamic_range_black_border", "dr_ev", "1"]),
            number(k, &["scene_range_ev"])
        ));
        rows_text.push(format!("Read noise {} DN · {} e-/DN (estimate)", number(k, &["dynamic_range_model", "read_noise_dn"]), number(k, &["dynamic_range_model", "e_per_dn"])));
    }
    if let Some(e) = results(Kind::Health) {
        let k = &e.metrics;
        let mut b = basis.clone();
        for p in b.rgba.chunks_exact_mut(4) {
            for c in 0..3 {
                p[c] /= 2;
            }
        }
        if let Some(eb) = e.layer("defects") {
            overlay(&mut b, eb, rb, 1.0);
        }
        tiles.push(tile(
            format!("Defects: hot {} · stuck {} · dead {}", number(k, &["count", "hot", "total"]), number(k, &["count", "stuck", "total"]), number(k, &["count", "dead", "total"])),
            &b,
        ));
        let border = &k["black_border"]["regions"][0];
        rows_text.push(format!(
            "Black border ({}): level {} DN, sigma {} DN, {} pattern ×{} · Banding image: columns {} / rows {} DN",
            number(k, &["black_border", "source"]),
            number(border, &["level_dn"]),
            number(border, &["sigma_dn"]),
            number(border, &["pattern_kind"]),
            number(border, &["pattern_factor"]),
            number(k, &["banding_image", "columns_sigma_dn"]),
            number(k, &["banding_image", "rows_sigma_dn"])
        ));
    }
    if let Some(e) = results(Kind::Lens) {
        let k = &e.metrics;
        let mut b = basis.clone();
        let mut title = String::from("Lens: ");
        if let Some(eb) = e.layer("vignette_profile") {
            overlay(&mut b, eb, rb, 0.8);
            title += &format!("Vignette corners {} EV ", number(k, &["profile", "vignette", "corners_ev"]));
        }
        if let Some(eb) = e.layer("distortion_grid") {
            overlay(&mut b, eb, rb, 1.0);
            title += &format!("· Distortion max {} px", number(k, &["profile", "distortion", "max_shift_px"]));
        }
        if e.layers.is_empty() {
            title += "no profile";
        }
        tiles.push(tile(title, &b));
        if let Some(eb) = e.layer("vignette_image") {
            let mut b = basis.clone();
            overlay(&mut b, eb, rb, 0.8);
            tiles.push(tile(format!("Vignette estimated from the image: corners {} EV", number(k, &["estimate_image", "corners_ev"])), &b));
        }
        rows_text.push(format!(
            "Lens: {} · Profile {} · Image estimate robust: {} ({})",
            number(k, &["lens", "exif"]),
            number(k, &["profile", "distortion", "kind"]),
            number(k, &["estimate_image", "robust"]),
            number(k, &["estimate_image", "corners_ev"])
        ));
    }
    if let Some(e) = results(Kind::Sharpness) {
        let k = &e.metrics;
        let mut b = basis.clone();
        for p in b.rgba.chunks_exact_mut(4) {
            for c in 0..3 {
                p[c] /= 2;
            }
        }
        if let Some(eb) = e.layer("sharpness") {
            overlay(&mut b, eb, rb, 1.0);
        }
        tiles.push(tile("Sharpness map (Tenengrad on green, before demosaicing)", &b));
        let mut b = basis.clone();
        if let Some(eb) = e.layer("peaking") {
            overlay(&mut b, eb, rb, 1.0);
        }
        tiles.push(tile(format!("Focus peaking ({} % of green pixels)", number(k, &["peaking_fraction_percent"])), &b));
        rows_text.push(format!("Sharpness: index {} · zones {} · sharpest point {}", number(k, &["sharpness_index"]), number(k, &["zones_relative"]), number(k, &["sharpest_point"])));
    }
    let mut timings = String::from("Times: ");
    for e in outcomes {
        timings += &format!("{} {:.0} ms · ", e.kind.name(), e.duration.as_secs_f64() * 1000.0);
    }
    rows_text.push(timings);

    // Layout: text header, then tiles in three columns
    let columns = 3;
    let width = columns * TILE + (columns + 1) * MARGIN;
    let header_h = 28 + rows_text.len() * 19 + MARGIN;
    let mut rows_h = Vec::new();
    for row_tiles in tiles.chunks(columns) {
        rows_h.push(row_tiles.iter().map(|k| k.canvas.height + 22).max().unwrap_or(0) + MARGIN);
    }
    let height = header_h + rows_h.iter().sum::<usize>() + MARGIN;
    let mut sheet = Canvas::new_at(width, height, [16, 16, 18, 255]);
    let title = rb.file.as_ref().and_then(|p| p.file_name()).map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
    sheet.text(MARGIN as f32, 6.0, 18.0, canvas::WHITE, &format!("Sensorblick – {title}"));
    for (i, z) in rows_text.iter().enumerate() {
        sheet.text(MARGIN as f32, 32.0 + i as f32 * 19.0, 13.0, canvas::TEXT, z);
    }
    let mut y = header_h;
    for (r, row_tiles) in tiles.chunks(columns).enumerate() {
        for (i, k) in row_tiles.iter().enumerate() {
            let x = MARGIN + i * (TILE + MARGIN);
            sheet.text(x as f32, y as f32, 12.0, canvas::TEXT, &k.title);
            sheet.insert_at(&k.canvas, x as isize, (y + 20) as isize);
        }
        y += rows_h[r];
    }
    sheet
}
