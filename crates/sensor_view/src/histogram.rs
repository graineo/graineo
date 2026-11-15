//! RAW histogram per filter class, saturation/headroom, clipping mask and comparison with the
//! rendered image (embedded camera JPEG or linear default exposure).
//!
//! The basis is a 16-bit histogram (65536 steps on 0…1, up to 1.0625) per class over the
//! crop – from it follow the percentiles, the EV view (1/8 EV) and the linear one (256 steps).
//! **Saturation**: if the values of a class pile up at the maximum (more than 8× the density below),
//! the sensor clips there – that is the measured level; otherwise the white point applies nominally.

use rayon::prelude::*;
use serde_json::{json, Value};

use crate::canvas::{self, Canvas, Color};
use crate::{region_json, round_to, Layer, GridMap, Options, RawImage, Result, Role, Saturation};

/// Steps of the fine histogram (65536 per unit, up to 1.0625).
pub(crate) const HB: usize = 69632;

#[inline]
pub(crate) fn step_no(v: f32) -> usize {
    if v <= 0.0 {
        0
    } else {
        ((v * 65536.0) as usize).min(HB - 1)
    }
}

pub(crate) struct Histo16 {
    pub h: Vec<Vec<u32>>,
    pub below_zero: Vec<u64>,
    pub max: Vec<f32>,
    pub n: Vec<u64>,
    pub sum_total: Vec<f64>,
}

impl Histo16 {
    fn empty(nk: usize) -> Self {
        Self { h: vec![vec![0; HB]; nk], below_zero: vec![0; nk], max: vec![f32::MIN; nk], n: vec![0; nk], sum_total: vec![0.0; nk] }
    }

    fn plus(mut self, o: Self) -> Self {
        for k in 0..self.h.len() {
            for (a, b) in self.h[k].iter_mut().zip(&o.h[k]) {
                *a += b;
            }
            self.below_zero[k] += o.below_zero[k];
            self.max[k] = self.max[k].max(o.max[k]);
            self.n[k] += o.n[k];
            self.sum_total[k] += o.sum_total[k];
        }
        self
    }

    /// Value at fraction `p` (0…1) of class `k`.
    pub fn percentile(&self, k: usize, p: f64) -> f32 {
        let target = (self.n[k] as f64 * p).ceil() as u64;
        let mut s = 0u64;
        for (b, &c) in self.h[k].iter().enumerate() {
            s += c as u64;
            if s >= target.max(1) {
                return (b as f32 + 0.5) / 65536.0;
            }
        }
        self.max[k]
    }
}

pub(crate) fn histo16(rb: &RawImage) -> Histo16 {
    let nk = rb.classes.names.len();
    let z = rb.crop;
    let class_at = rb.classes.class_at;
    (z.y..z.y + z.h)
        .into_par_iter()
        .fold(
            || Histo16::empty(nk),
            |mut a, y| {
                let zr = &rb.norm[y * rb.width..(y + 1) * rb.width];
                let kr = &class_at[y % 6];
                let mut lmax = [f32::MIN; 4];
                let mut lsum = [0f64; 4];
                let mut ln = [0u64; 4];
                for x in z.x..z.x + z.w {
                    let k = kr[x % 6] as usize;
                    let v = zr[x];
                    a.h[k][step_no(v)] += 1;
                    if v < 0.0 {
                        a.below_zero[k] += 1;
                    }
                    lmax[k] = lmax[k].max(v);
                    lsum[k] += v as f64;
                    ln[k] += 1;
                }
                for k in 0..nk {
                    a.max[k] = a.max[k].max(lmax[k]);
                    a.sum_total[k] += lsum[k];
                    a.n[k] += ln[k];
                }
                a
            },
        )
        .reduce(|| Histo16::empty(nk), Histo16::plus)
}

pub(crate) fn determine_saturation(rb: &RawImage, h: &Histo16) -> Saturation {
    let nk = rb.classes.names.len();
    let mut s = Saturation { level: vec![1.0; nk], threshold: vec![1.0; nk], measured: vec![false; nk] };
    for k in 0..nk {
        let c = rb.classes.color[k];
        let m = h.max[k];
        if h.n[k] == 0 || !m.is_finite() {
            continue;
        }
        let mb = step_no(m);
        // Tolerance: 2 raw value steps, at least 0.05 % of the range
        let dn = 65536.0 / (rb.white_dn - rb.black_dn[c]).max(1.0);
        let tol = ((2.0 * dn).max(0.0005 * 65536.0)).ceil() as usize;
        let lo = mb.saturating_sub(tol);
        let top: u64 = h.h[k][lo..=mb].iter().map(|&v| v as u64).sum();
        let a = lo.saturating_sub(20 * tol);
        let below: u64 = h.h[k][a..lo].iter().map(|&v| v as u64).sum();
        let density = below as f64 / ((lo - a).max(1) as f64 / (mb - lo + 1) as f64);
        let measured = m > 0.25 && top >= 20 && top as f64 > 8.0 * density.max(1.0);
        s.measured[k] = measured;
        if measured {
            s.level[k] = m;
            s.threshold[k] = lo as f32 / 65536.0;
        } else {
            s.level[k] = 1.0;
            s.threshold[k] = 1.0 - tol as f32 / 65536.0;
        }
    }
    s
}

pub(crate) fn class_color(rb: &RawImage, k: usize) -> Color {
    match (rb.classes.names.len(), k) {
        (4, 0) | (3, 0) => canvas::RED,
        (4, 1) | (3, 1) => canvas::GREEN,
        (4, 2) => canvas::GREEN2,
        _ => canvas::BLUE,
    }
}

pub(crate) fn analyze(rb: &RawImage, o: &Options) -> Result<(Vec<Layer>, Value)> {
    let h = rb.histo();
    let s = rb.saturation().clone();
    let nk = rb.classes.names.len();
    let z = rb.crop;

    // EV steps (1/8 EV, −16 … +0.25) and linear ones (256 on 0…1.0625)
    const EV_MIN: f32 = -16.0;
    const EV_N: usize = 16 * 8 + 2;
    let mut ev_h = vec![vec![0u64; EV_N]; nk];
    let mut lin_h = vec![vec![0u64; 256]; nk];
    for k in 0..nk {
        let p = s.level[k].max(1e-6);
        for (b, &c) in h.h[k].iter().enumerate() {
            if c == 0 {
                continue;
            }
            lin_h[k][b * 256 / HB] += c as u64;
            if b == 0 {
                continue;
            }
            let v = (b as f32 + 0.5) / 65536.0;
            let e = ((v / p).log2() - EV_MIN) * 8.0;
            if e >= 0.0 {
                ev_h[k][(e as usize).min(EV_N - 1)] += c as u64;
            }
        }
    }

    // Metrics per class
    let mut classes_json = Vec::new();
    let mut clipped_total = 0u64;
    let mut n_total = 0u64;
    let mut headroom = f32::MAX;
    let mut headroom_color = [f32::MAX; 3];
    for k in 0..nk {
        let n = h.n[k].max(1);
        let sb = step_no(s.threshold[k]);
        let clip: u64 = h.h[k][sb..].iter().map(|&v| v as u64).sum();
        clipped_total += clip;
        n_total += h.n[k];
        let p999 = h.percentile(k, 0.999);
        let p9999 = h.percentile(k, 0.9999);
        // highest non-clipped value
        let max_free = (0..sb).rev().find(|&b| h.h[k][b] > 0).map(|b| (b as f32 + 0.5) / 65536.0).unwrap_or(0.0);
        let fraction = clip as f64 / n as f64;
        let hr = if fraction > 1e-3 { 0.0 } else { (s.level[k] / p999.max(1e-6)).log2().max(0.0) };
        let hr_max = if clip > 0 { 0.0 } else { (s.level[k] / max_free.max(1e-6)).log2().max(0.0) };
        headroom = headroom.min(hr);
        let c = rb.classes.color[k];
        headroom_color[c] = headroom_color[c].min(hr);
        let dn = |v: f32| round_to((v * (rb.white_dn - rb.black_dn[c]) + rb.black_dn[c]) as f64, 0);
        classes_json.push(json!({
            "name": rb.classes.names[k],
            "pixel": h.n[k],
            "mean": round_to(h.sum_total[k] / n as f64, 5),
            "median": round_to(h.percentile(k, 0.5) as f64, 5),
            "p99": round_to(h.percentile(k, 0.99) as f64, 5),
            "p999": round_to(p999 as f64, 5),
            "p9999": round_to(p9999 as f64, 5),
            "max": round_to(h.max[k] as f64, 5),
            "max_dn": dn(h.max[k]),
            "below_black_percent": round_to(h.below_zero[k] as f64 / n as f64 * 100.0, 3),
            "saturation": round_to(s.level[k] as f64, 5),
            "saturation_dn": dn(s.level[k]),
            "saturation_measured": s.measured[k],
            "clip_threshold": round_to(s.threshold[k] as f64, 5),
            "clipped": clip,
            "clipped_percent": round_to(fraction * 100.0, 4),
            "headroom_ev": round_to(hr as f64, 2),
            "headroom_ev_max": round_to(hr_max as f64, 2),
            "histogram_ev": ev_h[k],
            "histogram_linear": lin_h[k],
        }));
    }

    // Map: clipping mask (maximum over blocks) and block values for the render comparison
    let k = GridMap::new_at(z, o.size);
    let threshold = s.threshold.clone();
    let class_at = rb.classes.class_at;
    let pattern = rb.pattern;
    // per block: clipped pixels per class, sum/count/max per color
    #[derive(Clone, Copy, Default)]
    struct Blk {
        clip: [u32; 4],
        n: [u32; 4],
        sum: [f32; 3],
        cnt: [u32; 3],
        max: [f32; 3],
    }
    let mut blocks = vec![Blk::default(); k.mean * k.mh];
    blocks.par_chunks_mut(k.mean).enumerate().for_each(|(my, row)| {
        let (y0, y1) = k.rows(my);
        for y in y0..y1 {
            let zr = &rb.norm[y * rb.width..(y + 1) * rb.width];
            for (i, x) in (k.x0..k.x0 + k.w).enumerate() {
                let b = &mut row[k.xmap[i] as usize];
                let kl = class_at[y % 6][x % 6] as usize;
                let c = pattern[y % 6][x % 6] as usize;
                let v = zr[x];
                b.n[kl] += 1;
                if v >= threshold[kl] {
                    b.clip[kl] += 1;
                }
                b.sum[c] += v;
                b.cnt[c] += 1;
                b.max[c] = b.max[c].max(v);
            }
        }
    });
    let mut mask = Canvas::new_at(k.mean, k.mh, canvas::TRANSPARENT);
    let mut blocks_clipped = 0usize;
    for (i, b) in blocks.iter().enumerate() {
        let mut f = [0u8; 3];
        let mut all = true;
        let mut any_set = false;
        for kl in 0..nk {
            if b.clip[kl] > 0 {
                any_set = true;
                let c = rb.classes.color[kl];
                f[c] = 255;
            }
        }
        for c in 0..3 {
            all &= f[c] == 255;
        }
        if any_set {
            blocks_clipped += 1;
            let f = if all { [255, 255, 255] } else { f };
            let px = [f[0], f[1], f[2], 230];
            mask.set(i % k.mean, i / k.mean, px);
        }
    }

    // Comparison with the rendered image
    let sat_f = rb.saturation_color();
    let reserve_block = |b: &Blk| -> Option<f32> {
        // None = clipped in the RAW (more than a quarter of a class), otherwise headroom in EV
        for kl in 0..nk {
            if b.n[kl] > 0 && b.clip[kl] * 4 > b.n[kl] {
                return None;
            }
        }
        Some((0..3).map(|c| (sat_f[c] / b.max[c].max(1e-6)).log2()).fold(f32::MAX, f32::min).max(0.0))
    };
    let evaluate = |clipped_frac: &dyn Fn(usize) -> bool, layer: Option<&mut Canvas>| -> Value {
        let mut n = 0usize;
        let mut raw_clip = 0usize;
        let mut reserves = Vec::new();
        let mut eb = layer;
        for (i, b) in blocks.iter().enumerate() {
            if !clipped_frac(i) {
                continue;
            }
            n += 1;
            let r = reserve_block(b);
            match r {
                None => raw_clip += 1,
                Some(r) => reserves.push(r),
            }
            if let Some(e) = eb.as_deref_mut() {
                let f = match r {
                    None => [230, 40, 40, 210],
                    Some(r) if r < 0.5 => [240, 200, 40, 210],
                    Some(_) => [40, 210, 90, 210],
                };
                e.set(i % k.mean, i / k.mean, f);
            }
        }
        let recoverable = reserves.iter().filter(|&&r| r >= 0.5).count();
        let med = if reserves.is_empty() { 0.0 } else { crate::percentile(&mut reserves.clone(), 0.5) };
        json!({
            "blown_percent": round_to(n as f64 / blocks.len() as f64 * 100.0, 2),
            "of_which_raw_clipped_percent": round_to(raw_clip as f64 / n.max(1) as f64 * 100.0, 1),
            "of_which_with_reserve_from_0_5ev_percent": round_to(recoverable as f64 / n.max(1) as f64 * 100.0, 1),
            "reserve_median_ev": round_to(med as f64, 2),
        })
    };
    let mut layers = Vec::new();
    let mut comparison = serde_json::Map::new();
    // linear default exposure (without highlight compression): blown out if a channel is ≥ 1
    let g = rb.standard_ev().exp2();
    let wb = rb.wb;
    let lin_out = |i: usize| {
        let b = &blocks[i];
        (0..3).any(|c| b.cnt[c] > 0 && b.max[c] * wb[c] * g >= 1.0)
    };
    let mut v = evaluate(&lin_out, None);
    v["exposure_ev"] = json!(round_to(rb.standard_ev() as f64, 2));
    comparison.insert("standard_exposure_linear".into(), v);
    if o.render_comparison && rb.file.is_some() {
        let mask = jpeg_mask(rb, &k);
        match mask {
            Ok((off, (pw, ph))) => {
                let mut layer = Canvas::new_at(k.mean, k.mh, canvas::TRANSPARENT);
                let mut v = evaluate(&|i| off[i], Some(&mut layer));
                v["jpeg_size"] = json!([pw, ph]);
                comparison.insert("camera_jpeg".into(), v);
                layers.push(Layer { name: "jpeg_reserve".into(), role: Role::Overlay, reference: Some(k.reference(z)), canvas: layer });
            }
            Err(f) => {
                comparison.insert("camera_jpeg".into(), json!({ "error": f }));
            }
        }
    }

    // Charts
    let ev_img = chart_ev(rb, &ev_h, &s, &classes_json);
    let lin_img = chart_linear(rb, &lin_h, &s);
    layers.insert(0, Layer { name: "clipping".into(), role: Role::Overlay, reference: Some(k.reference(z)), canvas: mask });
    layers.insert(0, Layer { name: "histogram_linear".into(), role: Role::Chart, reference: None, canvas: lin_img });
    layers.insert(0, Layer { name: "histogram_ev".into(), role: Role::Chart, reference: None, canvas: ev_img });

    let headroom_color: Vec<f64> = headroom_color.iter().map(|&v| round_to(if v == f32::MAX { 0.0 } else { v } as f64, 2)).collect();
    let kz = json!({
        "crop": region_json(z),
        "ev_levels": {"min": EV_MIN, "step": 0.125, "count": EV_N},
        "linear_levels": {"min": 0.0, "max": 1.0625, "count": 256},
        "classes": classes_json,
        "headroom_ev": round_to(headroom as f64, 2),
        "headroom_ev_rgb": headroom_color,
        "clipped_percent": round_to(clipped_total as f64 / n_total.max(1) as f64 * 100.0, 4),
        "clipping_blocks_percent": round_to(blocks_clipped as f64 / blocks.len() as f64 * 100.0, 2),
        "map": {"width": k.mean, "height": k.mh},
        "render_compare": Value::Object(comparison),
        "hint": "Headroom = log2(saturation / 99.9 % value) per class; 0 if more than 0.1 % are clipped. Render comparison: blocks that are blown out in the camera JPEG (≥ 250 in one channel), and their reserve in the RAW (brightest pixel per colour against saturation).",
    });
    Ok((layers, kz))
}

/// Blown-out mask of the embedded camera JPEG at map size (cached per size).
fn jpeg_mask(rb: &RawImage, k: &GridMap) -> crate::JpegMask {
    if let Some(m) = rb.jpeg_masks.lock().unwrap().get(&(k.mean, k.mh)) {
        return m.clone();
    }
    let z = rb.crop;
    let m = match rb.with_raw(|r| r.preview()).flatten() {
        None => Err("keine eingebettete Vorschau".to_string()),
        Some(jpg) => {
            let (pw, ph) = (jpg.width() as usize, jpg.height() as usize);
            let a_img = z.w as f32 / z.h as f32;
            let a_jpg = pw as f32 / ph as f32;
            if (a_img / a_jpg - 1.0).abs() >= 0.08 {
                Err(format!("Seitenverhältnis der Vorschau {pw}×{ph} passt nicht zum Zuschnitt"))
            } else {
                let px = jpg.as_raw();
                let mut off = vec![false; k.mean * k.mh];
                off.par_chunks_mut(k.mean).enumerate().for_each(|(my, row)| {
                    let (y0, y1) = (my * ph / k.mh, ((my + 1) * ph / k.mh).max(my * ph / k.mh + 1));
                    for (mx, o) in row.iter_mut().enumerate() {
                        let (x0, x1) = (mx * pw / k.mean, ((mx + 1) * pw / k.mean).max(mx * pw / k.mean + 1));
                        let (mut n, mut light_weight) = (0u32, 0u32);
                        for y in y0..y1.min(ph) {
                            for x in x0..x1.min(pw) {
                                let p = &px[(y * pw + x) * 3..][..3];
                                n += 1;
                                if p[0].max(p[1]).max(p[2]) >= 250 {
                                    light_weight += 1;
                                }
                            }
                        }
                        *o = n > 0 && light_weight * 2 >= n;
                    }
                });
                Ok((off, (pw, ph)))
            }
        }
    };
    rb.jpeg_masks.lock().unwrap().insert((k.mean, k.mh), m.clone());
    m
}

/// Axes and grid of a chart; returns the mappings x → px, y(0…1) → px.
pub(crate) struct Axes {
    pub l: f32,
    pub r: f32,
    pub o: f32,
    pub u: f32,
    pub x0: f32,
    pub x1: f32,
}

impl Axes {
    pub fn px(&self, x: f32) -> f32 {
        self.l + (x - self.x0) / (self.x1 - self.x0) * (self.r - self.l)
    }
    pub fn py(&self, t: f32) -> f32 {
        self.u - t.clamp(0.0, 1.0) * (self.u - self.o)
    }
}

pub(crate) fn chart_frame(b: &mut Canvas, a: &Axes, title: &str, xticks: &[(f32, String)], yticks: &[(f32, String)]) {
    b.text(a.l, 4.0, 13.0, canvas::TEXT, title);
    for (x, t) in xticks {
        let px = a.px(*x);
        b.line(px, a.o, px, a.u, canvas::RASTER);
        let tb = Canvas::text_width(11.0, t);
        b.text(px - tb / 2.0, a.u + 4.0, 11.0, canvas::TEXT, t);
    }
    for (y, t) in yticks {
        let py = a.py(*y);
        b.line(a.l, py, a.r, py, canvas::RASTER);
        let tb = Canvas::text_width(11.0, t);
        b.text(a.l - tb - 4.0, py - 7.0, 11.0, canvas::TEXT, t);
    }
    b.frame(a.l as isize, a.o as isize, (a.r - a.l) as usize + 1, (a.u - a.o) as usize + 1, [120, 120, 128, 255]);
}

fn curves(b: &mut Canvas, a: &Axes, rb: &RawImage, data: &[Vec<u64>], x_of: impl Fn(usize) -> f32) {
    // Height: square root of the count, scaled to the maximum of all classes (excluding the last step = clipping)
    let max = data.iter().flat_map(|d| d[..d.len() - 1].iter()).copied().max().unwrap_or(1).max(1) as f32;
    for (k, d) in data.iter().enumerate() {
        let f = class_color(rb, k);
        let pts: Vec<[f32; 2]> = d.iter().enumerate().map(|(i, &c)| [a.px(x_of(i)), a.py((c as f32 / max).sqrt())]).collect();
        let mut fl = f;
        fl[3] = 36;
        for p in &pts {
            if p[0] >= a.l && p[0] <= a.r {
                b.line(p[0], p[1], p[0], a.u, fl);
            }
        }
        let inner: Vec<[f32; 2]> = pts.into_iter().filter(|p| p[0] >= a.l - 1.0 && p[0] <= a.r + 1.0).collect();
        b.polyline(&inner, f);
    }
}

fn chart_ev(rb: &RawImage, ev_h: &[Vec<u64>], s: &Saturation, kj: &[Value]) -> Canvas {
    let (w, h) = (1024usize, 320usize);
    let mut b = Canvas::new_at(w, h, canvas::BACKGROUND);
    let a = Axes { l: 46.0, r: w as f32 - 14.0, o: 26.0, u: h as f32 - 50.0, x0: -14.0, x1: 0.5 };
    let xt: Vec<(f32, String)> = (-14..=0).map(|e| (e as f32, format!("{e}"))).collect();
    let yt = vec![(0.25f32, "6%".to_string()), (0.5, "25%".into()), (0.75, "56%".into()), (1.0, "100%".into())];
    chart_frame(&mut b, &a, "RAW-Histogramm je Filterklasse – EV relativ zur Sättigung (Höhe √Anzahl)", &xt, &yt);
    curves(&mut b, &a, rb, ev_h, |i| -16.0 + (i as f32 + 0.5) / 8.0);
    let p0 = a.px(0.0);
    b.line(p0, a.o, p0, a.u, [255, 255, 255, 200]);
    // Legend with headroom
    let mut x = a.l;
    for (k, kv) in kj.iter().enumerate() {
        let f = class_color(rb, k);
        b.rect(x as isize, (h - 22) as isize, 10, 10, f);
        let t = format!(
            "{} Headroom {:.2} EV, geclippt {:.3} %{}",
            rb.classes.names[k],
            kv["headroom_ev"].as_f64().unwrap_or(0.0),
            kv["clipped_percent"].as_f64().unwrap_or(0.0),
            if s.measured[k] { "" } else { " (nominell)" }
        );
        x += 14.0 + b.text(x + 14.0, (h - 26) as f32, 11.0, canvas::TEXT, &t) + 18.0;
    }
    b
}

fn chart_linear(rb: &RawImage, lin_h: &[Vec<u64>], s: &Saturation) -> Canvas {
    let (w, h) = (1024usize, 280usize);
    let mut b = Canvas::new_at(w, h, canvas::BACKGROUND);
    let a = Axes { l: 46.0, r: w as f32 - 14.0, o: 26.0, u: h as f32 - 30.0, x0: 0.0, x1: 1.0625 };
    let xt: Vec<(f32, String)> = (0..=4).map(|i| (i as f32 * 0.25, format!("{}%", i * 25))).collect();
    let yt = vec![(0.5f32, "25%".to_string()), (1.0, "100%".into())];
    chart_frame(&mut b, &a, "RAW-Histogramm linear (0–100 % des Weißpunkts, Höhe √Anzahl)", &xt, &yt);
    curves(&mut b, &a, rb, lin_h, |i| (i as f32 + 0.5) / 256.0 * 1.0625);
    for k in 0..s.level.len() {
        let mut f = class_color(rb, k);
        f[3] = 180;
        let px = a.px(s.level[k]);
        b.line(px, a.o, px, a.u, f);
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{analyze as analyse, Kind, Sensor};

    const RGGB: [[u8; 6]; 6] = {
        let mut m = [[0u8; 6]; 6];
        let mut y = 0;
        while y < 6 {
            let mut x = 0;
            while x < 6 {
                m[y][x] = match (y % 2, x % 2) {
                    (0, 0) => 0,
                    (1, 1) => 2,
                    _ => 1,
                };
                x += 1;
            }
            y += 1;
        }
        m
    };

    #[test]
    fn known_clipping() {
        // 200×100, left half 0.2, top right a 40×20 field with red clipped at 0.9 (saturation 0.9)
        let (w, h) = (200, 100);
        let mut m = vec![0.2f32; w * h];
        let mut n_clip = 0;
        for y in 0..h {
            for x in 0..w {
                let red = y % 2 == 0 && x % 2 == 0;
                if x >= 150 && x < 190 && y >= 10 && y < 30 && red {
                    m[y * w + x] = 0.9;
                    n_clip += 1;
                } else if red {
                    // gentle gradient, below saturation
                    m[y * w + x] = 0.1 + 0.5 * x as f32 / w as f32;
                }
            }
        }
        let rb = RawImage::from_mosaic(m, w, h, RGGB, Sensor::Bayer, [2.0, 1.0, 1.5]);
        let e = analyse(&rb, Kind::Histogram, &Options { size: 100, render_comparison: false, ..Default::default() }).unwrap();
        let r = &e.metrics["classes"][0];
        assert_eq!(r["clipped"].as_u64().unwrap(), n_clip as u64);
        assert!(r["saturation_measured"].as_bool().unwrap());
        assert!((r["saturation"].as_f64().unwrap() - 0.9).abs() < 1e-3);
        assert_eq!(r["headroom_ev"].as_f64().unwrap(), 0.0);
        // Green: not clipped, nominal white point 1 → headroom log2(1/0.2) ≈ 2.32
        let g = &e.metrics["classes"][1];
        assert_eq!(g["clipped"].as_u64().unwrap(), 0);
        assert!((g["headroom_ev"].as_f64().unwrap() - 2.32).abs() < 0.02);
        // Mask: only in the clipped field (map 1:2)
        let mk = e.layer("clipping").unwrap();
        let b = &mk.canvas;
        assert_eq!(b.width, 100);
        let ans1 = |x: usize, y: usize| b.get(x, y)[3] > 0;
        assert!(ans1(85, 10));
        assert!(!ans1(20, 20));
        assert!(!ans1(85, 30));
        assert_eq!(b.get(85, 10)[0], 255);
        assert_eq!(b.get(85, 10)[2], 0);
    }
}
