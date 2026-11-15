//! CFA mosaic view: raw values of a detail area in their filter color, nearest neighbor,
//! 400–1600 %, optionally with pixel grid and pattern tiles (Bayer 2×2, X-Trans 6×6).
//!
//! The pattern is taken from rawler and is relative to the sensor origin; tiles are
//! therefore drawn aligned to sensor coordinates (crop offset visible).

use serde_json::{json, Value};

use crate::canvas::Canvas;
use crate::{region_json, pattern_text, round_to, Region, Layer, Options, RawImage, Result, Role};

pub(crate) fn analyze(rb: &RawImage, o: &Options) -> Result<(Vec<Layer>, Value)> {
    let zoom = o.zoom.clamp(1, 32);
    let max_px = (4096 / zoom).max(8);
    let mut a = rb.detail(o, 48.min(max_px), 36.min(max_px));
    a.w = a.w.min(max_px);
    a.h = a.h.min(max_px);
    let z = rb.crop;
    let (sx0, sy0) = (z.x + a.x, z.y + a.y);
    let sat = rb.saturation().threshold.clone();
    let wb = rb.wb;

    // Display gain: 99 % value of the detail area (with WB) to 0.85
    let mut values: Vec<f32> = Vec::with_capacity(a.w * a.h);
    for y in sy0..sy0 + a.h {
        for x in sx0..sx0 + a.w {
            values.push(rb.value(x, y) * wb[rb.color(x, y)]);
        }
    }
    let p99 = crate::percentile(&mut values.clone(), 0.99).max(1e-4);
    let gain = (0.85 / p99).clamp(1.0, 256.0);

    let (bw, bh) = (a.w * zoom, a.h * zoom);
    let mut b = Canvas::new_at(bw, bh, crate::canvas::BLACK);
    let mut clipped = 0usize;
    for j in 0..a.h {
        for i in 0..a.w {
            let (x, y) = (sx0 + i, sy0 + j);
            let c = rb.color(x, y);
            let v = rb.value(x, y);
            let d = (raw::srgb_oetf((v * wb[c] * gain).clamp(0.0, 1.0)) * 255.0 + 0.5) as u8;
            let mut f = [0u8, 0, 0, 255];
            f[c] = d;
            // Pure blue looks very dark – brighten slightly, the color stays unambiguous
            if c == 2 {
                f[1] = d / 4;
            }
            b.rect((i * zoom) as isize, (j * zoom) as isize, zoom, zoom, f);
            let clip = v >= sat[rb.class(x, y)];
            if clip {
                clipped += 1;
                if zoom >= 6 {
                    b.frame((i * zoom + 1) as isize, (j * zoom + 1) as isize, zoom - 2, zoom - 2, [255, 255, 255, 255]);
                }
            }
        }
    }
    if o.grid && zoom >= 4 {
        let per = rb.period;
        for i in 0..=a.w {
            let tile = (sx0 + i) % per == 0;
            let f = if tile { [150, 150, 160, 255] } else { [0, 0, 0, 200] };
            let px = (i * zoom).min(bw - 1) as f32;
            b.line(px, 0.0, px, (bh - 1) as f32, f);
        }
        for j in 0..=a.h {
            let tile = (sy0 + j) % per == 0;
            let f = if tile { [150, 150, 160, 255] } else { [0, 0, 0, 200] };
            let py = (j * zoom).min(bh - 1) as f32;
            b.line(0.0, py, (bw - 1) as f32, py, f);
        }
    }

    let per = rb.period;
    let mut kz = json!({
        "crop": region_json(a),
        "sensor": {"sx": sx0, "sy": sy0},
        "zoom_percent": zoom * 100,
        "sensor_type": if per == 2 { "Bayer" } else { "X-Trans" },
        "period": per,
        "cfa_rawler": rb.info.as_ref().map(|i| i.cfa.clone()),
        "pattern_sensor": pattern_text(&rb.pattern, 0, 0, per),
        "pattern_image": pattern_text(&rb.pattern, z.x, z.y, per),
        "pattern_crop": pattern_text(&rb.pattern, sx0, sy0, per),
        "phase_crop": [sx0 % per, sy0 % per],
        "phase_crop": [z.x % per, z.y % per],
        "display_gain_ev": round_to(gain.log2() as f64, 2),
        "clipped": clipped,
        "display": "raw value × WB × gain, sRGB curve, in filter colour; white outline = clipped; bright lines = pattern tiles (aligned to sensor coordinates)",
    });
    if a.w * a.h <= 64 * 64 {
        let dn: Vec<u32> = (0..a.h)
            .flat_map(|j| (0..a.w).map(move |i| (sx0 + i, sy0 + j)))
            .map(|(x, y)| {
                let c = rb.color(x, y);
                (rb.value(x, y) * (rb.white_dn - rb.black_dn[c]) + rb.black_dn[c]).round().max(0.0) as u32
            })
            .collect();
        kz["values_dn"] = json!(dn);
        kz["colors"] = json!(pattern_text_region(rb, Region { x: sx0, y: sy0, w: a.w, h: a.h }));
    }
    let layers = vec![Layer { name: "mosaic".into(), role: Role::View, reference: Some(a), canvas: b }];
    Ok((layers, kz))
}

fn pattern_text_region(rb: &RawImage, r: Region) -> Vec<String> {
    (r.y..r.y + r.h).map(|y| (r.x..r.x + r.w).map(|x| ['R', 'G', 'B'][rb.color(x, y)]).collect()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{analyze as analyse, Kind, Sensor};

    /// X-Trans pattern (Fujifilm X-Pro1/X-T1, rawler "GGRGGBGGBGGRBRGRBGGGBGGRGGRGGBRBGBRG")
    fn xtrans() -> [[u8; 6]; 6] {
        let s = b"GGRGGBGGBGGRBRGRBGGGBGGRGGRGGBRBGBRG";
        std::array::from_fn(|y| std::array::from_fn(|x| match s[y * 6 + x] {
            b'R' => 0,
            b'G' => 1,
            _ => 2,
        }))
    }

    #[test]
    fn pattern_and_offset() {
        let (w, h) = (60, 48);
        let m = xtrans();
        let mut rb = RawImage::from_mosaic(vec![0.3; w * h], w, h, m, Sensor::XTrans, [1.0; 3]);
        // Crop with offset (3, 1): the pattern in the image starts there
        rb.crop = Region { x: 3, y: 1, w: 54, h: 42 };
        let o = Options { detail: Some(Region { x: 0, y: 0, w: 12, h: 12 }), zoom: 4, ..Default::default() };
        let e = analyse(&rb, Kind::Mosaic, &o).unwrap();
        let mb = e.metrics["pattern_image"].as_array().unwrap();
        // Row 1 of the sensor pattern from column 3: "GGBGGR"[3..] + [..3] = "GGRGGB"
        assert_eq!(mb[0].as_str().unwrap(), "GGRGGB");
        assert_eq!(e.metrics["phase_crop"], json!([3, 1]));
        // Color of the first pixel in the image: sensor (3,1) = 'G' → green pixel
        let b = &e.layers[0].canvas;
        assert_eq!(b.width, 48);
        let p = b.get(2, 2);
        assert!(p[1] > 0 && p[0] == 0);
        // Red pixel: sensor (5,1) = 'R' → image (2,0), mosaic pixel (2*4+2, 2)
        let p = b.get(10, 2);
        assert!(p[0] > 0 && p[1] == 0 && p[2] == 0);
        let colors = e.metrics["colors"].as_array().unwrap();
        assert_eq!(&colors[0].as_str().unwrap()[..6], "GGRGGB");
    }

    #[test]
    fn bayer_phase() {
        let (w, h) = (40, 40);
        let mut m = [[0u8; 6]; 6];
        // GRBG
        for y in 0..6 {
            for x in 0..6 {
                m[y][x] = match (y % 2, x % 2) {
                    (0, 0) | (1, 1) => 1,
                    (0, 1) => 0,
                    _ => 2,
                };
            }
        }
        let mut rb = RawImage::from_mosaic(vec![0.5; w * h], w, h, m, Sensor::Bayer, [1.0; 3]);
        rb.crop = Region { x: 1, y: 0, w: 38, h: 40 };
        let e = analyse(&rb, Kind::Mosaic, &Options { zoom: 4, ..Default::default() }).unwrap();
        assert_eq!(e.metrics["pattern_sensor"], json!(["GR", "BG"]));
        assert_eq!(e.metrics["pattern_image"], json!(["RG", "GB"]));
    }
}
