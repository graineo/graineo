//! Extended, encoded sRGB effects for wide-gamut exports. Shares scalar look helpers.
//! No quantization or output-gamut clipping here; those happen after ICC conversion.
use super::*;
fn apply_local(data:&mut[f32],w:usize,h:usize,s:&Settings)->Result<(),String>{
 if !crate::local::active(s){return Ok(())}
 // retouching (spots, removed objects, red eyes) first, as in `local::apply_view`
 crate::spots::apply_encoded(data,w,h,s);
 let Some(doc)=s.local.as_ref()else{return Ok(())};
 let linear:Vec<f32>=data.par_iter().map(|&v|graineo_color::srgb_decode(v)).collect();
 let result=masks::gpu::apply_auto(masks::Image::new(w,h,&linear)?,doc)?;
 data.par_iter_mut().zip(result).for_each(|(o,v)|*o=graineo_color::srgb_encode(v));Ok(())
}
fn local_contrast(data: &mut [f32], w:usize, h:usize, s: &Settings, geo: Geo) {
    if w < 4 || h < 4 {
        return;
    }
    let luma: Vec<f32> = data
        .par_chunks(3)
        .map(|p| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2])
        .collect();
    let mut target = luma.clone();
    if s.clarity != 0.0 {
        // Radius ≈ 0.8 % of the image width (≈ 38 px at 4704 px), in output pixels
        let r = ((geo.full_w as f32 * 0.008) / geo.scale.max(1.0) / 1.7).round().max(1.0) as usize;
        let base = blur(&luma, w, h, r);
        let k = s.clarity.clamp(-1.0, 1.0) * 0.8;
        target.par_iter_mut().zip(luma.par_iter().zip(base.par_iter())).for_each(|(t, (&l, &b))| {
            // Emphasise midtones, spare highlights and shadows
            let m = (4.0 * l * (1.0 - l)).clamp(0.0, 1.0);
            *t = l + k * m * (l - b);
        });
    }
    // Sharpening only visible from a radius ≥ 0.5 output pixels (hardly in the downscaled preview)
    let sr = s.sharpen_radius.clamp(0.5, 3.0) / geo.scale.max(1.0);
    if s.sharpen > 0.0 && sr >= 0.45 {
        let base = blur(&target, w, h, sr.round().max(1.0) as usize);
        let amount = s.sharpen.clamp(0.0, 2.0) * 1.2;
        let mask = s.sharpen_mask.clamp(0.0, 1.0);
        // Edge mask: gradient magnitude of the luminance
        let edge: Vec<f32> = (0..w * h)
            .into_par_iter()
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let at = |xx: usize, yy: usize| target[yy * w + xx];
                let gx = at((x + 1).min(w - 1), y) - at(x.saturating_sub(1), y);
                let gy = at(x, (y + 1).min(h - 1)) - at(x, y.saturating_sub(1));
                (gx * gx + gy * gy).sqrt()
            })
            .collect();
        let thr = mask * 0.08;
        target.par_iter_mut().enumerate().for_each(|(i, t)| {
            let m = if mask > 0.0 { ((edge[i] - thr * 0.5) / thr.max(1e-4)).clamp(0.0, 1.0) } else { 1.0 };
            *t += amount * m * (*t - base[i]);
        });
    }
    data.par_chunks_mut(3).zip(luma.par_iter().zip(target.par_iter())).for_each(|(p, (&l, &t))| {
        if l <= 1e-5 {
            return;
        }
        let f = (t.max(0.0) / l).clamp(0.0, 4.0);
        for v in p.iter_mut() {
            *v = *v * f;
        }
    });
}

pub fn apply(data: &mut [f32], width:usize, height:usize, s: &Settings, geo: Geo) -> Result<(),String> {
    if s.tone_eq_mask {
        return Ok(());
    }
    apply_local(data,width,height,s)?;
    // Remove colour fringes before all look steps (own detection on the output of the
    // Sigma tone curve, see there); no effect without an amount
    crate::defringe::wide::apply(data,width,height,s,geo);
    if !active(s) {
        return Ok(());
    }
    if s.clarity != 0.0 || s.sharpen > 0.0 {
        local_contrast(data,width,height,s,geo);
    }
    let grades = [s.grade_shadows, s.grade_midtones, s.grade_highlights].map(|[h, sat]| {
        let o = hue_offset(h);
        o.map(|c| c * sat.clamp(0.0, 1.0) * 0.35)
    });
    // Tone curve as a table (None = diagonal, step skipped). It acts as the first
    // colour step after clarity/sharpening: as in common photo apps it belongs to the base tonality on
    // which colour mixer and color grading build (so the grading zones follow the
    // brightness you see). Vignette, grain and matte shadows are effects on top; a
    // steep curve should not push the raised black point back down.
    let curve = crate::curve::CurveLut::new(&s.tone_curve);
    let hsl_on = !s.monochrome && s.hsl.iter().flatten().any(|&v| v != 0.0);
    let skin_on = crate::skin::active(s);
    let grading = s.grade_shadows[1] > 0.0 || s.grade_midtones[1] > 0.0 || s.grade_highlights[1] > 0.0;
    let pivot = (0.5 - 0.3 * s.grade_balance).clamp(0.15, 0.85);
    let fade = s.fade.clamp(0.0, 0.4);
    let frame = crate::geometry::Frame::from_settings(s, geo.full_w, geo.full_h, geo.camera);
    let vig = s.vignette.clamp(-1.0, 1.0);
    let vmid = s.vignette_mid.clamp(0.0, 1.0);
    // Grain size relative to the image width (at 4704 px: size 1 ≈ 1.6 px)
    let cell = (s.grain_size.clamp(0.3, 4.0) * 1.6 * geo.full_w as f32 / 4704.0).max(0.6);
    // In the preview fine grains average out; dampen the strength accordingly
    let grain_amp = s.grain_amount.clamp(0.0, 1.0) * 0.16 * (cell / geo.scale.max(1.0)).min(1.0).sqrt();
    let rough = s.grain_rough.clamp(0.0, 1.0);
    // skin-tone weights after tone curve and colour mixer, smoothed edge-preserving (empty when the stage is off)
    let skin_w: Vec<f32> = if skin_on {
        let pre = |i: usize| {
            let mut c = [data[i * 3], data[i * 3 + 1], data[i * 3 + 2]];
            if let Some(lut) = &curve {
                c = lut.apply(c);
            }
            if hsl_on {
                c = apply_hsl(c, &s.hsl);
            }
            c
        };
        let height = data.len() / 3 / width.max(1);
        crate::skin::weight_map(width, height, crate::skin::smooth_radius(geo.full_w, geo.full_h, geo.scale), pre)
    } else {
        Vec::new()
    };
    data.par_chunks_mut(width * 3).enumerate().for_each(|(y, line)| {
        let fy = geo.off_y as f32 + (y as f32 + 0.5) * geo.scale;
        for x in 0..width {
            let fx = geo.off_x as f32 + (x as f32 + 0.5) * geo.scale;
            let px = &mut line[x * 3..x * 3 + 3];
            let mut c = [px[0],px[1],px[2]];
            if let Some(lut) = &curve {
                c = lut.apply(c);
            }
            if hsl_on {
                c = apply_hsl(c, &s.hsl);
            }
            if skin_on {
                c = crate::skin::apply_weighted(c, s.skin_hue, s.skin_sat, skin_w[y * width + x]);
            }
            let l = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
            if grading {
                let ws = 1.0 - smoothstep(0.0, pivot, l);
                let wh = smoothstep(pivot, 1.0, l);
                let wm = (1.0 - ws - wh).max(0.0);
                for k in 0..3 {
                    c[k] += ws * grades[0][k] + wm * grades[1][k] + wh * grades[2][k];
                }
            }
            if vig != 0.0 {
                // "After crop": relative to the (rotated) crop
                let u = frame.from_src([fx, fy]);
                let (dx, dy) = ((u[0] - 0.5) * 2.0, (u[1] - 0.5) * 2.0);
                let d = ((dx * dx + dy * dy) * 0.5).sqrt();
                let f = smoothstep(vmid * 0.8, 1.05, d);
                for v in &mut c {
                    *v = if vig < 0.0 { *v * (1.0 + vig * f) } else { *v + vig * f * (1.0 - *v) };
                }
            }
            if grain_amp > 0.0 {
                let mut n = value_noise(fx, fy, cell, 0x5157);
                if rough > 0.0 {
                    n = n * (1.0 - 0.5 * rough) + rough * value_noise(fx, fy, cell * 0.5, 0xA11CE);
                }
                // Grain is most visible in the midtones, hardly in deep black and white
                let m = (4.0 * l * (1.0 - l)).clamp(0.0, 1.0).sqrt() * 0.85 + 0.15;
                let g = n * grain_amp * m;
                for v in &mut c {
                    *v += g;
                }
            }
            if fade > 0.0 {
                for v in &mut c {
                    *v = fade + *v * (1.0 - fade);
                }
            }
            for k in 0..3 {
                px[k] = c[k];
            }
        }
    });
    Ok(())
}

