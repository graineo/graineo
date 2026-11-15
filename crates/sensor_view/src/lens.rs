//! Lens: vignetting as a brightness map and distortion as a grid.
//!
//! - **Profile**: the correction the development would use (`raw::RawFile::lens_correction`,
//!   sources own profile → LCP profile/backup → camera → lensfun; without automatic backup).
//!   The maps are made by applying the correction to helper images: to an all-ones image
//!   (vignette only) → gain 1/falloff; to coordinate ramps (geometry only, zoom 1) →
//!   source coordinate per target pixel. Straight lines in reality (target grid) appear in the RAW
//!   along the source coordinates – so the grid shows the distortion over the uncorrected image.
//! - **Estimate from the image**: radial polynomial log G = c₀ + c₂r² + c₄r⁴ over flat, non-
//!   clipped blocks; only "robust" if the model explains the blocks well and the corners are covered.

use serde_json::{json, Value};

use crate::canvas::{self, Canvas};
use crate::{round_to, Region, Layer, GridMap, Options, RawImage, Result, Role};

pub(crate) fn analyze(rb: &RawImage, o: &Options) -> Result<(Vec<Layer>, Value)> {
    let z = rb.crop;
    let k = GridMap::new_at(z, o.size);
    let reference = Region { x: 0, y: 0, w: z.w, h: z.h };
    let mut layers = Vec::new();
    let oo = raw::lens::LensOptions { backup_enabled: false, ..Default::default() };
    let corr = rb.with_raw(|r| r.lens_correction(z.w, z.h, &oo)).flatten();
    let mut profile = Value::Null;
    let mut vignette_profile: Option<Vec<f32>> = None;
    if let Some(kr) = &corr {
        let mut pj = serde_json::Map::new();
        // Vignette
        if let Some(v) = &kr.vignette {
            let mut only = kr.clone();
            only.geometry = None;
            only.zoom = 1.0;
            let mut px = vec![[1f32; 3]; k.mean * k.mh];
            only.apply_scaled(&mut px, k.mean, k.mh);
            let ev: Vec<f32> = px.iter().map(|p| -(p[1].max(1e-6)).log2()).collect();
            let corner = [0, k.mean - 1, (k.mh - 1) * k.mean, k.mh * k.mean - 1].iter().map(|&i| ev[i]).sum::<f32>() / 4.0;
            let edge_mid = [k.mean / 2, (k.mh - 1) * k.mean + k.mean / 2, (k.mh / 2) * k.mean, (k.mh / 2) * k.mean + k.mean - 1].iter().map(|&i| ev[i]).sum::<f32>() / 4.0;
            pj.insert(
                "vignette".into(),
                json!({"source": v.source, "kind": v.kind.short(), "corners_ev": round_to(corner as f64, 2), "edge_center_ev": round_to(edge_mid as f64, 2)}),
            );
            layers.push(Layer { name: "vignette_profile".into(), role: Role::Overlay, reference: Some(reference), canvas: vignette_img(&ev, k.mean, k.mh) });
            vignette_profile = Some(ev);
        }
        // Distortion
        if let Some(g) = &kr.geometry {
            let mut only = kr.clone();
            only.vignette = None;
            let auto_zoom = only.zoom;
            only.zoom = 1.0;
            // Ramps in full image coordinates (pixel center)
            let sx = z.w as f32 / k.mean as f32;
            let sy = z.h as f32 / k.mh as f32;
            let mut rx: Vec<[f32; 3]> = (0..k.mean * k.mh).map(|i| [((i % k.mean) as f32 + 0.5) * sx - 0.5; 3]).collect();
            let mut ry: Vec<[f32; 3]> = (0..k.mean * k.mh).map(|i| [((i / k.mean) as f32 + 0.5) * sy - 0.5; 3]).collect();
            only.apply_scaled(&mut rx, k.mean, k.mh);
            only.apply_scaled(&mut ry, k.mean, k.mh);
            // Shift per map pixel (full pixels), lateral error R/B against G
            let mut max_v = 0f32;
            let mut corner_v = 0f32;
            let mut ca = [0f32; 2];
            for my in 0..k.mh {
                for mx in 0..k.mean {
                    let i = my * k.mean + mx;
                    let (tx, ty) = ((mx as f32 + 0.5) * sx - 0.5, (my as f32 + 0.5) * sy - 0.5);
                    let d = (rx[i][1] - tx).hypot(ry[i][1] - ty);
                    max_v = max_v.max(d);
                    if (mx == 0 || mx == k.mean - 1) && (my == 0 || my == k.mh - 1) {
                        corner_v += d / 4.0;
                        ca[0] = ca[0].max((rx[i][0] - rx[i][1]).hypot(ry[i][0] - ry[i][1]));
                        ca[1] = ca[1].max((rx[i][2] - rx[i][1]).hypot(ry[i][2] - ry[i][1]));
                    }
                }
            }
            // Grid: target lines → source coordinates (green), at map scale
            let mut b = Canvas::new_at(k.mean, k.mh, canvas::TRANSPARENT);
            let (nx, ny) = (16usize, (16 * k.mh / k.mean).max(4));
            let source = |mx: usize, my: usize| -> [f32; 2] {
                let i = my.min(k.mh - 1) * k.mean + mx.min(k.mean - 1);
                [(rx[i][1] + 0.5) / sx - 0.5, (ry[i][1] + 0.5) / sy - 0.5]
            };
            let straight = [255, 255, 255, 70];
            let curve = [80, 255, 140, 230];
            for i in 0..=nx {
                let mx = (i * (k.mean - 1)) / nx;
                b.line(mx as f32, 0.0, mx as f32, (k.mh - 1) as f32, straight);
                let pts: Vec<[f32; 2]> = (0..k.mh).step_by(2).map(|my| source(mx, my)).collect();
                b.polyline(&pts, curve);
            }
            for j in 0..=ny {
                let my = (j * (k.mh - 1)) / ny;
                b.line(0.0, my as f32, (k.mean - 1) as f32, my as f32, straight);
                let pts: Vec<[f32; 2]> = (0..k.mean).step_by(2).map(|mx| source(mx, my)).collect();
                b.polyline(&pts, curve);
            }
            let label = format!("{} – {}", g.kind.short(), g.source);
            b.rect(4, 4, (Canvas::text_width(12.0, &label) + 8.0) as usize, 18, [0, 0, 0, 150]);
            b.text(8.0, 5.0, 12.0, [255, 255, 255, 255], &label);
            pj.insert(
                "distortion".into(),
                json!({
                    "source": g.source, "kind": g.kind.short(),
                    "max_shift_px": round_to(max_v as f64, 1),
                    "corners_shift_px": round_to(corner_v as f64, 1),
                    "lateral_error_corner_px": {"r_g": round_to(ca[0] as f64, 2), "b_g": round_to(ca[1] as f64, 2)},
                    "auto_zoom": round_to(auto_zoom as f64, 4),
                    "grid": "white = straight target grid, green = where these lines lie in the uncorrected RAW",
                }),
            );
            layers.push(Layer { name: "distortion_grid".into(), role: Role::Overlay, reference: Some(reference), canvas: b });
        }
        profile = Value::Object(pj);
    }

    // Estimate from the image
    let estimate_val = estimate(rb, &k);
    if let Some((ev, _)) = &estimate_val.1 {
        layers.push(Layer { name: "vignette_image".into(), role: Role::Overlay, reference: Some(reference), canvas: vignette_img(ev, k.mean, k.mh) });
    }
    let comparison = match (&vignette_profile, &estimate_val.1) {
        (Some(p), Some((s, _))) => {
            let d = p.iter().zip(s).map(|(a, b)| (a - b).abs()).sum::<f32>() / p.len() as f32;
            json!(round_to(d as f64, 3))
        }
        _ => Value::Null,
    };
    let kz = json!({
        "profile": profile,
        "profile_present": corr.is_some(),
        "lens": rb.info.as_ref().map(|i| json!({"exif": i.exif.lens, "rawler": i.exif.lens_db, "focal_length": i.exif.focal_length, "aperture": i.exif.aperture})),
        "estimate_image": estimate_val.0,
        "deviation_profile_image_ev": comparison,
        "map": {"width": k.mean, "height": k.mh},
        "maps": {"vignette_*": "Helligkeitsabfall in EV (0 = Mitte, Skala bis −2 EV)", "distortion_grid": "Gitter-Overlay mit Quellenetikett"},
    });
    Ok((layers, kz))
}

fn vignette_img(ev: &[f32], w: usize, h: usize) -> Canvas {
    let mut b = Canvas::new_at(w, h, canvas::TRANSPARENT);
    for (i, &e) in ev.iter().enumerate() {
        // 0 EV (no shading) … −2 EV
        let t = (-e / 2.0).clamp(0.0, 1.0);
        let c = canvas::heat(1.0 - t);
        let a = (40.0 + 170.0 * t) as u8;
        b.set(i % w, i / w, [c[0], c[1], c[2], a]);
    }
    // Contour lines every ½ EV
    for y in 0..h.saturating_sub(1) {
        for x in 0..w.saturating_sub(1) {
            let s = |v: f32| (v * 2.0).floor();
            let a = s(ev[y * w + x]);
            if a != s(ev[y * w + x + 1]) || a != s(ev[(y + 1) * w + x]) {
                b.set(x, y, [255, 255, 255, 230]);
            }
        }
    }
    b
}

/// Radial model from flat blocks: (metrics, Some((EV map, R²)) if robust).
fn estimate(rb: &RawImage, k: &GridMap) -> (Value, Option<(Vec<f32>, f32)>) {
    // coarse grid (≈ 48 blocks across the long edge) from the map averaging
    let gk = GridMap::new_at(rb.crop, 48);
    let m = rb.block_mean(&gk);
    let sat = rb.saturation_color();
    let (gw, gh) = (gk.mean, gk.mh);
    let r_of = |x: f32, y: f32, w: usize, h: usize| {
        let (cx, cy) = ((w as f32 - 1.0) / 2.0, (h as f32 - 1.0) / 2.0);
        ((x - cx).hypot(y - cy)) / cx.hypot(cy)
    };
    // flat: neighbors differ little (< 6 %), not clipped, not too dark
    let mut pts_acc: Vec<(f32, f32)> = Vec::new();
    let mut corners = 0;
    for y in 1..gh.saturating_sub(1) {
        for x in 1..gw.saturating_sub(1) {
            let g = m[y * gw + x][1];
            if g < 0.01 || g > 0.8 * sat[1] {
                continue;
            }
            let nb = [m[y * gw + x - 1][1], m[y * gw + x + 1][1], m[(y - 1) * gw + x][1], m[(y + 1) * gw + x][1]];
            if nb.iter().all(|&v| (v / g - 1.0).abs() < 0.06) {
                let r = r_of(x as f32, y as f32, gw, gh);
                pts_acc.push((r, g.ln()));
                if r > 0.8 {
                    corners += 1;
                }
            }
        }
    }
    let n = pts_acc.len();
    if n < 30 || corners < 4 {
        return (json!({"robust": false, "base": format!("too few flat blocks ({n}, of which {corners} at the edge)")}), None);
    }
    // least squares: y = c0 + c2 r² + c4 r⁴
    let mut a = [[0f64; 3]; 3];
    let mut bv = [0f64; 3];
    for &(r, y) in &pts_acc {
        let f = [1.0, (r * r) as f64, (r * r * r * r) as f64];
        for i in 0..3 {
            for j in 0..3 {
                a[i][j] += f[i] * f[j];
            }
            bv[i] += f[i] * y as f64;
        }
    }
    let Some(c) = solve_3x3(a, bv) else { return (json!({"robust": false, "base": "singular system of equations"}), None) };
    let model = |r: f32| c[0] + c[1] * (r * r) as f64 + c[2] * (r * r * r * r) as f64;
    let my = pts_acc.iter().map(|p| p.1 as f64).sum::<f64>() / n as f64;
    let ss_tot: f64 = pts_acc.iter().map(|p| (p.1 as f64 - my).powi(2)).sum();
    let ss_res: f64 = pts_acc.iter().map(|p| (p.1 as f64 - model(p.0)).powi(2)).sum();
    let r2 = 1.0 - ss_res / ss_tot.max(1e-12);
    let corner_ev = ((model(1.0) - c[0]) / std::f64::consts::LN_2) as f32;
    // monotonically decreasing and plausible (0 … −4 EV)
    let monotonic = (1..=10).all(|i| model(i as f32 / 10.0) <= model((i - 1) as f32 / 10.0) + 1e-3);
    // real vignetting: same brightness at the same radius → small spread per radius ring
    // (a portrait with a bright face against a dark background would otherwise seem to fit the model)
    let mut rings = [(0f64, 0f64, 0u32); 5];
    for &(r, y) in &pts_acc {
        let i = ((r * 5.0) as usize).min(4);
        let d = y as f64 - model(r);
        rings[i].0 += d;
        rings[i].1 += d * d;
        rings[i].2 += 1;
    }
    let occupied = rings.iter().filter(|r| r.2 >= 5).count();
    let spread = rings.iter().filter(|r| r.2 >= 5).map(|r| (r.1 / r.2 as f64 - (r.0 / r.2 as f64).powi(2)).max(0.0).sqrt()).fold(0.0, f64::max);
    let robust = r2 > 0.6 && monotonic && (-4.0..=-0.05).contains(&corner_ev) && occupied >= 4 && spread < 0.12;
    let kz = json!({"robust": robust, "blocks": n, "border_blocks": corners, "r2": round_to(r2, 3), "corners_ev": round_to(corner_ev as f64, 2), "monotone": monotonic, "rings_occupied": occupied, "ring_spread_ln": round_to(spread, 3),
                    "model": "ln G = c0 + c2·r² + c4·r⁴ (r = 1 an der Ecke)", "c": c.map(|v| round_to(v, 5))});
    if !robust {
        return (kz, None);
    }
    let ev: Vec<f32> = (0..k.mean * k.mh).map(|i| ((model(r_of((i % k.mean) as f32, (i / k.mean) as f32, k.mean, k.mh)) - c[0]) / std::f64::consts::LN_2) as f32).collect();
    (kz, Some((ev, r2 as f32)))
}

fn solve_3x3(a: [[f64; 3]; 3], b: [f64; 3]) -> Option<[f64; 3]> {
    let det = |m: &[[f64; 3]; 3]| m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0]) + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    let d = det(&a);
    if d.abs() < 1e-12 {
        return None;
    }
    Some(std::array::from_fn(|k| {
        let mut m = a;
        for i in 0..3 {
            m[i][k] = b[i];
        }
        det(&m) / d
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Sensor;

    #[test]
    fn vignette_from_flat_image() {
        // flat area with cos⁴-like falloff: −1 EV at the corners
        let (w, h) = (600, 400);
        let mut m = vec![0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let (cx, cy) = ((w as f32 - 1.0) / 2.0, (h as f32 - 1.0) / 2.0);
                let r = ((x as f32 - cx).hypot(y as f32 - cy)) / cx.hypot(cy);
                m[y * w + x] = 0.4 * (-(r * r) * std::f32::consts::LN_2).exp();
            }
        }
        let rggb = std::array::from_fn(|y| std::array::from_fn(|x| match (y % 2, x % 2) {
            (0, 0) => 0,
            (1, 1) => 2,
            _ => 1,
        }));
        let rb = RawImage::from_mosaic(m, w, h, rggb, Sensor::Bayer, [1.0; 3]);
        let k = GridMap::new_at(rb.crop, 200);
        let (kz, map) = estimate(&rb, &k);
        assert!(kz["robust"].as_bool().unwrap(), "{kz}");
        assert!((kz["corners_ev"].as_f64().unwrap() + 1.0).abs() < 0.15, "{kz}");
        assert!(map.is_some());
    }
}
