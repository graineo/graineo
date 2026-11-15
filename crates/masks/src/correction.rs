use super::*;
/// Neutral is 0 everywhere; EV for exposure, normalized sliders otherwise.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Adjustments {
    pub exposure: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub temperature: f32,
    pub tint: f32,
    pub saturation: f32,
    pub clarity: f32,
    pub texture: f32,
    pub sharpness: f32,
    pub noise_reduction: f32,
}
impl Adjustments {
    pub fn validate(&self) -> Result<()> {
        if !self.exposure.is_finite() || self.exposure.abs() > 10.0 {
            return Err("Belichtung außerhalb ±10 EV".into());
        }
        if [
            self.contrast,
            self.highlights,
            self.shadows,
            self.temperature,
            self.tint,
            self.saturation,
            self.clarity,
            self.texture,
        ]
        .iter()
        .any(|v| !v.is_finite() || v.abs() > 1.0)
            || !unit(self.sharpness)
            || !unit(self.noise_reduction)
        {
            return Err("Korrekturparameter außerhalb des Bereichs".into());
        }
        Ok(())
    }
}
/// Pure function; layers follow in stored order, each mask before its adjustment.
pub fn apply(image: Image<'_>, document: &Document) -> Result<Vec<f32>> {
    apply_view(image, document, View::whole(&image))
}

/// Filter radii (pixels) of the spatial planes of one layer: fine, medium (texture), wide (clarity).
pub(crate) fn radii(short: usize) -> [usize; 3] {
    let short = short as f32;
    [1, (short * 0.004).round().max(1.0) as usize, (short * 0.016).round().max(1.0) as usize]
}

/// Margin in pixels a crop needs around it so that [`apply_view`] gives the same result inside the
/// crop as on the whole image (`full` pixels): the reach of the spatial filters of all layers added up
/// (each layer reads the output of the previous one).
pub fn margin(document: &Document, full: [usize; 2]) -> usize {
    let short = full[0].min(full[1]);
    let r = radii(short);
    let scale = (short as f32 / 1000.0).max(0.001);
    document
        .layers
        .iter()
        .map(|l| {
            let a = l.adjustments;
            if a.clarity == 0.0 && a.texture == 0.0 && a.sharpness == 0.0 && a.noise_reduction == 0.0 {
                return 0;
            }
            if document.version == 2 {
                let mut m = r[0];
                if a.texture != 0.0 {
                    m = m.max(r[1]);
                }
                if a.clarity != 0.0 {
                    m = m.max(r[2]);
                }
                m
            } else {
                // bilinear taps at ±radius
                (scale * 16.0).ceil() as usize + 2
            }
        })
        .sum()
}

/// Like [`apply`] for a crop of a larger image (see [`View`]): masks and filter sizes follow the
/// whole image; filters are cut at the edges of the crop (enough margin: [`margin`]).
pub fn apply_view(image: Image<'_>, document: &Document, view: View) -> Result<Vec<f32>> {
    document.validate()?;
    let mut current: std::borrow::Cow<'_, [f32]> = std::borrow::Cow::Borrowed(image.data);
    for layer in &document.layers {
        let mask = Prepared::new(&layer.mask, view.full[0], view.full[1])?;
        let a = layer.adjustments;
        if a == Adjustments::default() {
            continue;
        }
        let base_exposure = 2.0f32.powf(a.exposure);
        let gains = [
            2.0f32.powf(0.5 * a.temperature - 0.25 * a.tint),
            2.0f32.powf(0.5 * a.tint),
            2.0f32.powf(-0.5 * a.temperature - 0.25 * a.tint),
        ];
        let spatial =
            a.clarity != 0.0 || a.texture != 0.0 || a.sharpness != 0.0 || a.noise_reduction != 0.0;
        // Separate immutable input and output prevents neighborhood feedback and tile seams.
        let mut output = vec![0.0; current.len()];
        let planes = if spatial && document.version == 2 {
            Some(crate::filters::planes(
                &current,
                image.width,
                image.height,
                [
                    a.noise_reduction != 0.0 || a.sharpness != 0.0,
                    a.texture != 0.0,
                    a.clarity != 0.0,
                ],
                radii(view.short()),
            ))
        } else {
            None
        };
        let scale = (view.short() as f32 / 1000.0).max(0.001);
        output.par_chunks_mut(3).enumerate().for_each(|(i, out)| {
            let x = i % image.width;
            let y = i / image.width;
            let p = &current[i * 3..i * 3 + 3];
            let weight = mask.sample(view.point(x, y), p);
            if weight == 0.0 {
                out.copy_from_slice(p);
                return;
            }
            let lum = luma(p).max(0.0);
            let tonal = 1.0
                + a.shadows * (1.0 - smooth(lum / 0.5))
                + a.highlights * smooth((lum - 0.25) / 0.75);
            let exposure = if a.shadows == 0.0 && a.highlights == 0.0 {
                base_exposure
            } else {
                base_exposure * 2.0f32.powf(tonal - 1.0)
            };
            let mut q = [0.; 3];
            for c in 0..3 {
                let mut v = p[c];
                if spatial {
                    let fine = planes.as_ref().map_or_else(
                        || blur(&current, image.width, image.height, x, y, c, 1.0),
                        |b| {
                            if b[0].is_empty() {
                                p[c]
                            } else {
                                b[0][i * 3 + c]
                            }
                        },
                    );
                    let medium = if a.texture != 0.0 {
                        planes.as_ref().map_or_else(
                            || blur(&current, image.width, image.height, x, y, c, scale * 4.0),
                            |b| {
                                if b[1].is_empty() {
                                    p[c]
                                } else {
                                    b[1][i * 3 + c]
                                }
                            },
                        )
                    } else {
                        fine
                    };
                    let wide = if a.clarity != 0.0 {
                        planes.as_ref().map_or_else(
                            || blur(&current, image.width, image.height, x, y, c, scale * 16.0),
                            |b| {
                                if b[2].is_empty() {
                                    p[c]
                                } else {
                                    b[2][i * 3 + c]
                                }
                            },
                        )
                    } else {
                        fine
                    };
                    // Range-limited denoising protects strong edges; unsharp residuals retain sign/HDR.
                    let delta = fine - v;
                    v += a.noise_reduction * delta / (1.0 + (delta / 0.03).powi(2));
                    let limit = |d: f32, t: f32| {
                        if document.version == 2 {
                            d / (1.0 + (d / t).powi(2))
                        } else {
                            d
                        }
                    };
                    v += a.sharpness * limit(p[c] - fine, 0.025)
                        + a.texture * limit(p[c] - medium, 0.04) * 0.5
                        + a.clarity * limit(p[c] - wide, 0.08) * 0.5;
                }
                v *= exposure * gains[c];
                // Signed power contrast about 18% gray, continuous through black.
                if a.contrast != 0.0 {
                    v = 0.18 * (v / 0.18).abs().powf(1.0 + 0.5 * a.contrast) * v.signum();
                }
                q[c] = v;
            }
            let yq = luma(&q);
            for c in 0..3 {
                let v = if a.saturation == 0.0 {
                    q[c]
                } else {
                    yq + (q[c] - yq) * (1.0 + a.saturation)
                };
                out[c] = p[c] + weight * (v - p[c]);
            }
        });
        if !output.par_iter().all(|v| v.is_finite()) {
            return Err("Korrektur überschreitet den f32-Wertebereich".into());
        }
        current = std::borrow::Cow::Owned(output);
    }
    Ok(current.into_owned())
}
// Fixed separable binomial 3×3 kernel with bilinear taps, radius relative to short image edge.
// Clamped boundary extension; no mask applied to neighbors (prevents halos from mask borders).
fn blur(data: &[f32], w: usize, h: usize, x: usize, y: usize, c: usize, radius: f32) -> f32 {
    let center = data[(y * w + x) * 3 + c];
    let mut sum = 0.0;
    for (dy, wy) in [(-radius, 0.25), (0.0, 0.5), (radius, 0.25)] {
        for (dx, wx) in [(-radius, 0.25), (0.0, 0.5), (radius, 0.25)] {
            let fx = (x as f32 + dx).clamp(0.0, (w - 1) as f32);
            let fy = (y as f32 + dy).clamp(0.0, (h - 1) as f32);
            let x0 = fx.floor() as usize;
            let y0 = fy.floor() as usize;
            let x1 = (x0 + 1).min(w - 1);
            let y1 = (y0 + 1).min(h - 1);
            let tx = fx - x0 as f32;
            let ty = fy - y0 as f32;
            let tl = data[(y0 * w + x0) * 3 + c];
            let tr = data[(y0 * w + x1) * 3 + c];
            let bl = data[(y1 * w + x0) * 3 + c];
            let br = data[(y1 * w + x1) * 3 + c];
            let top = tl + (tr - tl) * tx;
            let bot = bl + (br - bl) * tx;
            sum += (top + (bot - top) * ty - center) * wx * wy;
        }
    }
    center + sum
}

#[cfg(test)]
mod view_tests {
    use super::*;
    /// A crop with the margin of [`margin`] gives the same values as the whole image inside the crop.
    #[test]
    fn crop_with_margin_matches_whole_image() {
        let (w, h) = (240usize, 160usize);
        let data: Vec<f32> = (0..w * h * 3)
            .map(|i| 0.2 + 0.15 * ((i as f32 * 0.37).sin() + ((i / (w * 3)) as f32 * 0.11).cos()))
            .collect();
        let doc = Document::from_json(
            r#"{"version":2,"layers":[{"mask":{"type":"radial","center":[0.5,0.5],"radii":[0.3,0.25],"angle":0.4,"feather":0.5},"adjustments":{"exposure":0.5,"clarity":0.4}},{"mask":{"type":"brush","points":[[0.3,0.3],[0.5,0.45],[0.7,0.5]],"radius":0.05,"hardness":0.5,"flow":0.6},"adjustments":{"texture":0.5,"noise_reduction":0.3}}]}"#,
        )
        .unwrap();
        let whole = apply(Image::new(w, h, &data).unwrap(), &doc).unwrap();
        let m = margin(&doc, [w, h]);
        assert!(m >= 3);
        let (x, y, cw, ch) = (70usize, 40usize, 60usize, 50usize);
        let (x0, y0) = (x - m, y - m);
        let (ew, eh) = (cw + 2 * m, ch + 2 * m);
        let crop: Vec<f32> = (y0..y0 + eh).flat_map(|yy| data[(yy * w + x0) * 3..(yy * w + x0 + ew) * 3].to_vec()).collect();
        let view = View { full: [w, h], offset: [x0, y0] };
        let part = apply_view(Image::new(ew, eh, &crop).unwrap(), &doc, view).unwrap();
        for yy in 0..ch {
            for xx in 0..cw {
                for c in 0..3 {
                    let a = whole[((y + yy) * w + x + xx) * 3 + c];
                    let b = part[((m + yy) * ew + m + xx) * 3 + c];
                    assert!((a - b).abs() < 1e-5, "{a} {b} at {xx},{yy}");
                }
            }
        }
    }
}
