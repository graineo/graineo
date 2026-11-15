//! Grain loupe: upscale the scene-linear crop before the film computation.
//! Own bilinear interpolation, no third-party GUI/Python components.
use crate::{look::Geo, Linear};

/// Bilinear in linear values. Factor 1 stays unchanged; at most twelve and 32 MP.
pub fn upscale(input: &Linear, factor: usize) -> Result<Linear, String> {
    if factor == 1 {
        return Ok(input.clone());
    }
    let (w, h) = (
        input.width.checked_mul(factor),
        input.height.checked_mul(factor),
    );
    let (Some(w), Some(h)) = (w, h) else {
        return Err("Ausschnitt zu groß".into());
    };
    if !(1..=12).contains(&factor)
        || w.saturating_mul(h) > 32_000_000
        || input.width == 0
        || input.height == 0
    {
        return Err("Ausschnitt zu groß".into());
    }
    use rayon::prelude::*;
    let mut data = vec![0.0; w * h * 3];
    data.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        let sy = ((y as f32 + 0.5) / factor as f32 - 0.5).clamp(0.0, (input.height - 1) as f32);
        let y0 = sy.floor() as usize;
        let y1 = (y0 + 1).min(input.height - 1);
        let fy = sy - y0 as f32;
        for (x, out) in row.chunks_exact_mut(3).enumerate() {
            let sx = ((x as f32 + 0.5) / factor as f32 - 0.5).clamp(0.0, (input.width - 1) as f32);
            let x0 = sx.floor() as usize;
            let x1 = (x0 + 1).min(input.width - 1);
            let fx = sx - x0 as f32;
            for c in 0..3 {
                let a = input.data[(y0 * input.width + x0) * 3 + c];
                let b = input.data[(y0 * input.width + x1) * 3 + c];
                let d = input.data[(y1 * input.width + x0) * 3 + c];
                let e = input.data[(y1 * input.width + x1) * 3 + c];
                out[c] = (a + (b - a) * fx) * (1.0 - fy) + (d + (e - d) * fx) * fy;
            }
        }
    });
    Ok(Linear { mono: false, haze: None, toneq: None,
        width: w,
        height: h,
        data,
        detail: None,
    })
}
pub fn geometry(full: [usize; 2], origin: [usize; 2], camera: u8, factor: usize) -> Geo {
    Geo {
        full_w: full[0],
        full_h: full[1],
        off_x: origin[0],
        off_y: origin[1],
        scale: 1.0 / factor.max(1) as f32,
        camera,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interpolation_and_geometry() {
        let a = Linear { mono: false, haze: None, toneq: None,
            width: 2,
            height: 1,
            data: vec![0., 0., 0., 1., 1., 1.],
            detail: None,
        };
        assert_eq!(upscale(&a, 1).unwrap().data, a.data);
        let b = upscale(&a, 2).unwrap();
        assert_eq!((b.width, b.height), (4, 2));
        assert_eq!(
            &b.data[..12],
            &[0., 0., 0., 0.25, 0.25, 0.25, 0.75, 0.75, 0.75, 1., 1., 1.]
        );
        assert_eq!(geometry([6000, 4000], [100, 200], 0, 12).scale, 1. / 12.);
        assert!(upscale(&a, 13).is_err());
    }
    #[test]
    fn fixed_seed_and_format() {
        let scene = Linear { mono: false, haze: None, toneq: None,
            width: 96,
            height: 64,
            data: vec![0.184; 96 * 64 * 3],
            detail: None,
        };
        let f = crate::film::FilmSettings {
            active: true,
            stock: "ilford_hp5_plus".into(),
            auto_balance: false,
            halation: 0.,
            couplers: 0.,
            ..Default::default()
        };
        let a = crate::film::develop(&scene, &f, geometry([6000, 4000], [200, 100], 0, 4)).unwrap();
        let b = crate::film::develop(&scene, &f, geometry([6000, 4000], [200, 100], 0, 4)).unwrap();
        assert_eq!(a.data, b.data);
        let variance = |im: &crate::Rgb16| {
            let x: Vec<f64> = im.data.chunks_exact(3).map(|p| p[0] as f64).collect();
            let m = x.iter().sum::<f64>() / x.len() as f64;
            (x.iter().map(|v| (v - m).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
        };
        let normal =
            crate::film::develop(&scene, &f, geometry([6000, 4000], [200, 100], 0, 1)).unwrap();
        eprintln!(
            "grain variance 1×={:.3}, 4×={:.3}",
            variance(&normal),
            variance(&a)
        );
        assert!(variance(&a).is_finite());
        assert!(variance(&a) < variance(&normal) * 2.0);
    }
}
