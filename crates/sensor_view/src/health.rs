//! Sensor health: hot/dead/stuck pixels, black level per channel, optical black border,
//! banding (row/column patterns).
//!
//! Defects are tested statistically against the same-color neighbors in a 5×5 window (Bayer:
//! 8 or 12, X-Trans: 4–12 depending on position):
//! - **hot**: value > 3 × largest same-color neighbor *and* > neighbor + max(8 σ, 1 %), neighbors
//!   below 25 % (in bright surroundings single spikes are usually subject detail), and
//!   the direct neighbors of other colors (with white balance) are clearly darker as well (otherwise it is
//!   a real point of light);
//! - **stuck** (bright): clipped while all same-color neighbors are below half saturation;
//! - **dead**: value < ¼ of the smallest neighbor with sufficient light (neighbors > 5 %).
//! A single image can only detect "stuck" as "isolated saturated".

use rayon::prelude::*;
use serde_json::{json, Value};

use crate::canvas::{self, Canvas};
use crate::noise::black_borders;
use crate::{region_json, same_color_neighbors, round_to, Region, Layer, GridMap, Options, RawImage, Result, Role};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DefectKind {
    Hot,
    Stuck,
    Dead,
}

impl DefectKind {
    fn name(&self) -> &'static str {
        match self {
            DefectKind::Hot => "hot",
            DefectKind::Stuck => "stuck",
            DefectKind::Dead => "dead",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Defect {
    x: usize,
    y: usize,
    type_name: DefectKind,
    value: f32,
    neighbor: f32,
}

fn find_defects(rb: &RawImage) -> (Vec<Defect>, u64) {
    let z = rb.crop;
    let nb = same_color_neighbors(&rb.pattern, 2);
    let sat: Vec<f32> = rb.saturation().threshold.clone();
    let rm = rb.noise_model();
    let sigma = |v: f32| rm.map(|r| (r.a[1] * v.max(0.0) + r.b[1]).max(0.0).sqrt()).unwrap_or(0.002);
    let wb = rb.wb;
    let w = rb.width;
    let (x0, x1) = (z.x.max(2), (z.x + z.w).min(w - 2));
    let (y0, y1) = (z.y.max(2), (z.y + z.h).min(rb.height - 2));
    let outcomes: Vec<(Vec<Defect>, u64)> = (y0..y1)
        .into_par_iter()
        .map(|y| {
            let mut v_out = Vec::new();
            let mut checkable = 0u64;
            let n = &rb.norm;
            for x in x0..x1 {
                let v = n[y * w + x];
                let lst = &nb[(y % 6) * 6 + x % 6];
                // fast rejection: first neighbor
                let (dx, dy) = lst[0];
                let first_entry = n[(y as isize + dy) as usize * w + (x as isize + dx) as usize];
                let hot_possible = v > 2.0 * first_entry && v > 0.004;
                let dead_possible = first_entry > 0.05 && v < 0.25 * first_entry;
                if first_entry > 0.05 {
                    checkable += 1;
                }
                if !hot_possible && !dead_possible {
                    continue;
                }
                let mut mx = f32::MIN;
                let mut mn = f32::MAX;
                for &(dx, dy) in lst {
                    let u = n[(y as isize + dy) as usize * w + (x as isize + dx) as usize];
                    mx = mx.max(u);
                    mn = mn.min(u);
                }
                let c = rb.color(x, y);
                // in bright surroundings (sky through branches, specular highlights) single spikes are usually real
                if hot_possible && mx < 0.25 && v > 3.0 * mx && v > mx + (8.0 * sigma(mx)).max(0.01) {
                    // direct neighbors (any color, with WB) must be clearly darker
                    let mut direct = 0f32;
                    for dy in -1isize..=1 {
                        for dx in -1isize..=1 {
                            if (dx, dy) != (0, 0) {
                                let (xx, yy) = ((x as isize + dx) as usize, (y as isize + dy) as usize);
                                direct = direct.max(n[yy * w + xx] * wb[rb.color(xx, yy)]);
                            }
                        }
                    }
                    if v * wb[c] > 1.5 * direct {
                        let type_name = if v >= sat[rb.class(x, y)] && mx < 0.5 * sat[rb.class(x, y)] { DefectKind::Stuck } else { DefectKind::Hot };
                        v_out.push(Defect { x, y, type_name, value: v, neighbor: mx });
                    }
                } else if dead_possible && mn > 0.05 && v < 0.25 * mn && v < mn - 8.0 * sigma(mn) {
                    v_out.push(Defect { x, y, type_name: DefectKind::Dead, value: v, neighbor: mn });
                }
            }
            (v_out, checkable)
        })
        .collect();
    let mut all = Vec::new();
    let mut checkable = 0;
    for (v, p) in outcomes {
        all.extend(v);
        checkable += p;
    }
    (all, checkable)
}

/// Robust pattern level: median per column (or row) of the same-color high-pass.
/// `columns = true`: column pattern (vertical stripes). Result in normalized units.
fn pattern_profile(rb: &RawImage, columns: bool) -> (Vec<f32>, f32) {
    let z = rb.crop;
    let w = rb.width;
    // Distance to the nearest same-color pixel across the profile direction
    let d_cross = |x: usize, y: usize| -> Option<(usize, usize)> {
        let c = rb.color(x, y);
        if columns {
            let l = (1..=6).find(|&d| x >= d && rb.color(x - d, y) == c)?;
            let r = (1..=6).find(|&d| x + d < w && rb.color(x + d, y) == c)?;
            Some((l, r))
        } else {
            let l = (1..=6).find(|&d| y >= d && rb.color(x, y - d) == c)?;
            let r = (1..=6).find(|&d| y + d < rb.height && rb.color(x, y + d) == c)?;
            Some((l, r))
        }
    };
    let n_prof = if columns { z.w } else { z.h };
    let values: Vec<f32> = (0..n_prof)
        .into_par_iter()
        .map(|i| {
            let mut v = Vec::with_capacity(2048);
            let length = if columns { z.h } else { z.w };
            let advance = (length / 1500).max(1);
            let mut j = 6;
            while j + 6 < length {
                let (x, y) = if columns { (z.x + i, z.y + j) } else { (z.x + j, z.y + i) };
                if (columns && (x < 6 || x + 6 >= w)) || (!columns && (y < 6 || y + 6 >= rb.height)) {
                    j += advance;
                    continue;
                }
                if let Some((l, r)) = d_cross(x, y) {
                    let (a, b) = if columns { (rb.value(x - l, y), rb.value(x + r, y)) } else { (rb.value(x, y - l), rb.value(x, y + r)) };
                    let (fl, fr) = (r as f32 / (l + r) as f32, l as f32 / (l + r) as f32);
                    v.push(rb.value(x, y) - fl * a - fr * b);
                }
                j += advance;
            }
            if v.is_empty() {
                0.0
            } else {
                crate::percentile(&mut v, 0.5)
            }
        })
        .collect();
    let m = values.iter().sum::<f32>() / values.len().max(1) as f32;
    let s = (values.iter().map(|v| (v - m).powi(2)).sum::<f32>() / values.len().max(1) as f32).sqrt();
    // High-pass with weights ≈ ½/½: Var(d) ≈ 1.5 Var(pattern)
    (values, s / 1.5f32.sqrt())
}

pub(crate) fn analyze(rb: &RawImage, o: &Options) -> Result<(Vec<Layer>, Value)> {
    let z = rb.crop;
    let t = std::time::Instant::now();
    let (mut defects, checkable) = find_defects(rb);
    let t_defects = t.elapsed().as_secs_f64() * 1000.0;
    defects.sort_by(|a, b| (b.value / b.neighbor.max(1e-4)).total_cmp(&(a.value / a.neighbor.max(1e-4))));
    let mut counter = serde_json::Map::new();
    for type_name in [DefectKind::Hot, DefectKind::Stuck, DefectKind::Dead] {
        let mut per = [0usize; 3];
        for d in defects.iter().filter(|d| d.type_name == type_name) {
            per[rb.color(d.x, d.y)] += 1;
        }
        counter.insert(type_name.name().into(), json!({"total": per.iter().sum::<usize>(), "r": per[0], "g": per[1], "b": per[2]}));
    }
    let list: Vec<Value> = defects
        .iter()
        .map(|d| {
            let c = rb.color(d.x, d.y);
            let dn = |v: f32| (v * (rb.white_dn - rb.black_dn[c]) + rb.black_dn[c]).round();
            json!({"x": d.x - z.x, "y": d.y - z.y, "sx": d.x, "sy": d.y, "type": d.type_name.name(), "color": (["R", "G", "B"][c]),
                   "value_dn": dn(d.value), "neighbor_dn": dn(d.neighbor)})
        })
        .collect();

    // Overlay: circles around defects
    let k = GridMap::new_at(z, o.size);
    let mut ov = Canvas::new_at(k.mean, k.mh, canvas::TRANSPARENT);
    let fx = k.mean as f32 / z.w as f32;
    for d in defects.iter().rev() {
        let f = match d.type_name {
            DefectKind::Hot => [255, 90, 40, 255],
            DefectKind::Stuck => [255, 40, 220, 255],
            DefectKind::Dead => [40, 220, 255, 255],
        };
        let (cx, cy) = ((d.x - z.x) as f32 * fx, (d.y - z.y) as f32 * fx);
        ov.circle(cx, cy, 5.0, f);
        ov.circle(cx, cy, 6.0, [0, 0, 0, 160]);
        ov.set(cx as usize, cy as usize, f);
    }

    // Black level per class (table from the file)
    let info = rb.info.as_ref();
    let black_classes: Vec<Value> = (0..rb.classes.names.len())
        .map(|kl| {
            let mut s = 0f32;
            let mut n = 0f32;
            if let Some(i) = info {
                let (bw, bh, bc) = i.black_pattern;
                let (bw, bh, bc) = (bw.max(1), bh.max(1), bc.max(1));
                for y in 0..6 {
                    for x in 0..6 {
                        if rb.classes.class_at[y][x] as usize == kl {
                            s += i.black.get(((y % bh) * bw + x % bw) * bc).copied().unwrap_or(i.black.first().copied().unwrap_or(0.0));
                            n += 1.0;
                        }
                    }
                }
            }
            json!({"class": rb.classes.names[kl], "black_dn": round_to((s / n.max(1.0)) as f64, 1)})
        })
        .collect();

    // Optical black border: level and deviation from the black level used, row/column noise
    let (borders, source) = black_borders(rb);
    let border_json: Vec<Value> = borders
        .iter()
        .map(|r| {
            let mut s = [0f64; 3];
            let mut s2 = [0f64; 3];
            let mut n = [0u64; 3];
            let vertical = r.h > r.w;
            let mut profile = vec![0f64; if vertical { r.h } else { r.w }];
            let mut pn = vec![0u32; profile.len()];
            for y in r.y..(r.y + r.h).min(rb.height) {
                for x in r.x..(r.x + r.w).min(rb.width) {
                    let c = rb.color(x, y);
                    let v = rb.value(x, y) as f64 * (rb.white_dn - rb.black_dn[c]) as f64;
                    s[c] += v;
                    s2[c] += v * v;
                    n[c] += 1;
                    let i = if vertical { y - r.y } else { x - r.x };
                    profile[i] += v;
                    pn[i] += 1;
                }
            }
            let mean: [f64; 3] = std::array::from_fn(|c| s[c] / n[c].max(1) as f64);
            let sigma: [f64; 3] = std::array::from_fn(|c| (s2[c] / n[c].max(1) as f64 - mean[c] * mean[c]).max(0.0).sqrt());
            let p: Vec<f64> = profile.iter().zip(&pn).map(|(s, &n)| s / n.max(1) as f64).collect();
            let pm = p.iter().sum::<f64>() / p.len().max(1) as f64;
            let ps = (p.iter().map(|v| (v - pm).powi(2)).sum::<f64>() / p.len().max(1) as f64).sqrt();
            let across = if vertical { r.w } else { r.h };
            let expected = sigma[1] / (across as f64).sqrt();
            json!({
                "region": region_json(*r),
                "orientation": if vertical { "senkrecht (Zeilenrauschen messbar)" } else { "waagerecht (Spaltenrauschen messbar)" },
                "mean_above_black_dn": mean.map(|v| round_to(v, 2)),
                "level_dn": ([0, 1, 2].map(|c| round_to(mean[c] + rb.black_dn[c] as f64, 1))),
                "sigma_dn": sigma.map(|v| round_to(v, 2)),
                "pattern_noise_dn": round_to(ps, 3),
                "pattern_kind": if vertical { "Zeilen" } else { "Spalten" },
                "expected_no_pattern_dn": round_to(expected, 3),
                "pattern_factor": round_to(ps / expected.max(1e-9), 2),
            })
        })
        .collect();

    // Banding in the image (robust, median per column/row)
    let scale = (rb.white_dn - rb.black_dn[1]) as f64;
    let (sp_profile, sp_sigma) = pattern_profile(rb, true);
    let (ze_profile, ze_sigma) = pattern_profile(rb, false);
    let read_noise = rb.noise_model().map(|r| r.b[1].sqrt() as f64 * scale);
    let downscale = |p: &[f32]| -> Vec<f64> {
        let n = 256.min(p.len());
        (0..n).map(|i| round_to(p[i * p.len() / n] as f64 * scale, 3)).collect()
    };

    let kz = json!({
        "pixel_checked": (z.w * z.h),
        "dead_checkable_percent": round_to(checkable as f64 / (z.w * z.h).max(1) as f64 * 100.0, 1),
        "count": Value::Object(counter),
        "defects": list,
        "defects_duration_ms": round_to(t_defects, 1),
        "black_level": {
            "file_dn": black_classes,
            "pattern": info.map(|i| json!({"values": i.black, "size": [i.black_pattern.0, i.black_pattern.1, i.black_pattern.2]})),
            "white_dn": rb.white_dn,
        },
        "black_border": {"source": if borders.is_empty() { "none" } else { source }, "regions": border_json},
        "banding_image": {
            "columns_sigma_dn": round_to(sp_sigma as f64 * scale, 3),
            "rows_sigma_dn": round_to(ze_sigma as f64 * scale, 3),
            "read_noise_dn": read_noise.map(|v| round_to(v, 2)),
            "columns_relative_to_read_noise": read_noise.map(|l| round_to(sp_sigma as f64 * scale / l.max(1e-9), 3)),
            "rows_relative_to_read_noise": read_noise.map(|l| round_to(ze_sigma as f64 * scale / l.max(1e-9), 3)),
            "column_profile_dn": downscale(&sp_profile),
            "row_profile_dn": downscale(&ze_profile),
            "hint": "Median per column/row of the same-colour high-pass; fixed image structures (fences, edges) can contribute. Fixed patterns below ~0.1 × read noise are invisible.",
        },
    });
    let layers = vec![Layer { name: "defects".into(), role: Role::Overlay, reference: Some(Region { x: 0, y: 0, w: z.w, h: z.h }), canvas: ov }];
    Ok((layers, kz))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{analyze as analyse, Kind, Sensor};

    #[test]
    fn known_defects() {
        let (w, h) = (120, 90);
        let rggb = std::array::from_fn(|y| std::array::from_fn(|x| match (y % 2, x % 2) {
            (0, 0) => 0,
            (1, 1) => 2,
            _ => 1,
        }));
        // slight pattern instead of completely flat (so nothing is equal by chance)
        let mut m: Vec<f32> = (0..w * h).map(|i| 0.2 + 0.01 * ((i * 7919) % 13) as f32 / 13.0).collect();
        m[40 * w + 30] = 0.95; // hot (G in row 40, column 30 → R row, even column = R)
        m[61 * w + 71] = 0.01; // dead (B)
        // real point of light over several pixels (not a defect)
        for y in 70..73 {
            for x in 90..93 {
                m[y * w + x] = 0.9;
            }
        }
        let rb = RawImage::from_mosaic(m, w, h, rggb, Sensor::Bayer, [1.0; 3]);
        let e = analyse(&rb, Kind::Health, &Options { size: 120, ..Default::default() }).unwrap();
        let d = e.metrics["defects"].as_array().unwrap();
        assert_eq!(d.len(), 2, "{d:?}");
        let hot = d.iter().find(|v| v["type"] == "hot").unwrap();
        assert_eq!((hot["x"].as_u64().unwrap(), hot["y"].as_u64().unwrap()), (30, 40));
        assert_eq!(hot["color"], "R");
        let dead = d.iter().find(|v| v["type"] == "dead").unwrap();
        assert_eq!((dead["x"].as_u64().unwrap(), dead["y"].as_u64().unwrap()), (71, 61));
        assert_eq!(dead["color"], "B");
        assert!(e.layers[0].canvas.get(30, 40)[3] > 0);
    }
}
