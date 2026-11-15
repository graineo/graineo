//! `graineo-dev classic-output FILE OUT.tif [--no-header] [--new-form|--old-form] [--wb NAME] [--mode NAME] [--sharpness S]`:
//! the new Classic chain to the linear codes, then the new output step
//! (`develop::classic_new::output`) to a 16-bit sRGB TIFF of the active area, rotated as the
//! file header says. The sliders come from the file (`x3f::sppa`) unless `--no-header` is given
//! (then neutral); `--sharpness` replaces the sharpness slider. `--new-form`/`--old-form` override the form of the tone table chosen for the camera
//! (test aid).

use anyhow::{Context, Result};
use develop::classic_new::{output, detail};
use rayon::prelude::*;
use x3f::X3fFile;

pub fn run(file: &str, args: &[String]) -> Result<()> {
    let outp = args.first().context("Ausgabedatei fehlt")?;
    let flag = |f: &str| args.iter().any(|a| a == f);
    let value = |f: &str| args.iter().position(|a| a == f).and_then(|i| args.get(i + 1)).cloned();
    let x = X3fFile::open(file)?;
    let camf = x.camf()?;
    let props = x.properties()?;
    let header = x3f::sppa::read_path(file);
    let slider = match (&header, flag("--no-header")) {
        (Some(a), false) => output::Slider::from_file(a),
        _ => output::Slider::default(),
    };
    let mut slider = slider;
    if let Some(v) = value("--sharpness").and_then(|v| v.parse::<f64>().ok()) {
        slider.sharpness = v;
    }
    let wb = value("--wb")
        .or_else(|| header.as_ref().and_then(|a| a.white_balance.clone()))
        .unwrap_or_else(|| props.get("WB_DESC").unwrap_or("Auto").trim().to_string());
    let mode = value("--mode").unwrap_or_else(|| props.get("CM_DESC").unwrap_or("Standard").trim().to_string());
    let rot = x.header_rotation().unwrap_or(0) as u32;
    let t = std::time::Instant::now();
    let codes = detail::with_sharpness(slider.sharpness, || develop::classic_new::run(&x, &camf, &props, &wb, &mode, rot)).map_err(anyhow::Error::msg)?;
    let t_chain = t.elapsed();
    let mut opt = output::Options::for_camera(&props);
    if flag("--new-form") {
        opt.new_form = true;
    }
    if flag("--old-form") {
        opt.new_form = false;
    }
    let a = output::Output::with_options(&camf, &props, &wb, &mode, false, &slider, &codes, opt).map_err(anyhow::Error::msg)?;
    let [x0, y0, x1, y1] = codes.crop;
    let (w, h) = (x1 - x0, y1 - y0);
    let mut px = vec![0u16; w * h * 3];
    px.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let p = ((y0 + y) * codes.width + x0 + x) * 3;
            let o = a.pixel([codes.data[p], codes.data[p + 1], codes.data[p + 2]]);
            row[x * 3..x * 3 + 3].copy_from_slice(&o);
        }
    });
    let (w, h, px) = rotate(w, h, px, rot);
    develop::export::write_tiff16(&develop::Rgb16 { width: w, height: h, data: px }, std::path::Path::new(outp))?;
    println!(
        "{file}: WB {wb}, mode {mode}, sliders {slider:?}, balance {:?}, mode contrast {}, chain {:.2?}, total {:.2?}",
        a.amplification,
        a.contrast_mode,
        t_chain,
        t.elapsed()
    );
    Ok(())
}

/// Quarter turns clockwise for the header rotation in degrees.
fn rotate(w: usize, h: usize, px: Vec<u16>, grad: u32) -> (usize, usize, Vec<u16>) {
    let q = (grad / 90) % 4;
    if q == 0 {
        return (w, h, px);
    }
    let (nw, nh) = if q % 2 == 1 { (h, w) } else { (w, h) };
    let mut out = vec![0u16; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let (nx, ny) = match q {
                1 => (h - 1 - y, x),
                2 => (w - 1 - x, h - 1 - y),
                _ => (y, w - 1 - x),
            };
            let s = (y * w + x) * 3;
            let d = (ny * nw + nx) * 3;
            out[d..d + 3].copy_from_slice(&px[s..s + 3]);
        }
    }
    let _ = nh;
    (nw, nh, out)
}
