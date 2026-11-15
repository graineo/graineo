//! `graineo-dev analog`: measurements for black-and-white film exposed from the Foveon layers
//! ("Sigma-Analog", see `develop::analog`).
//!
//! ```text
//! graineo-dev analog szene IMAGE.X3F out.f32 [--layers identity|none] [--wb NAME]
//!     scene-linear image before the film (middle grey 0.184) as raw f32 (w·h·3, row by row);
//!     with `--layers identity` the three white-balanced, normalised layers (bottom, middle, top)
//!     instead of linear sRGB. Prints size, camera, ISO and Sigma's matrix as JSON on stdout.
//! graineo-dev analog gewichte IMAGE.X3F --film <id> [--filter <id>] [--amount <0…1>]
//!     layer weights and expected noise for this photo (ISO) and film
//! ```

use anyhow::{bail, Context, Result};
use develop::{Session, Settings};

pub fn run(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("scene") => scene(&args[1..]),
        Some("weights") => weights(&args[1..]),
        _ => bail!("usage: graineo-dev analog szene|gewichte …"),
    }
}

fn scene(args: &[String]) -> Result<()> {
    let file = args.first().context("image missing")?;
    let out = args.get(1).context("output missing")?;
    let mut s = Settings::default();
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--layers" => {
                i += 1;
                s.layer_matrix = match args.get(i).map(String::as_str) {
                    Some("identity") => Some(develop::layers::IDENTITY),
                    _ => None,
                };
            }
            "--wb" => {
                i += 1;
                s.white_balance = args.get(i).cloned();
            }
            "--mode" => {
                i += 1;
                s.color_mode = args.get(i).cloned();
            }
            a => bail!("unknown option {a}"),
        }
        i += 1;
    }
    let mut session = Session::open(file)?;
    let lin = session.render_full_scene(&s, true).map_err(anyhow::Error::msg)?;
    let bytes: Vec<u8> = lin.data.iter().flat_map(|v| v.to_le_bytes()).collect();
    std::fs::write(out, bytes)?;
    let cal = &session.cal;
    let m = cal.cam_to_srgb(s.color_mode.as_deref(), s.white_balance.as_deref());
    let info = serde_json::json!({
        "width": lin.width, "height": lin.height, "camera": cal.camera, "iso": cal.iso,
        "wb": cal.wb_name, "cam_to_srgb": m.0, "wb_gain": cal.wb_gain, "noise_model": cal.noise_model,
        "orientation": cal.orientation,
    });
    println!("{info}");
    Ok(())
}

fn weights(args: &[String]) -> Result<()> {
    let file = args.first().context("image missing")?;
    let mut s = Settings::default();
    s.film.active = true;
    let mut i = 1;
    while i < args.len() {
        let v = args.get(i + 1).cloned().unwrap_or_default();
        match args[i].as_str() {
            "--film" => s.film.stock = v,
            "--filter" => s.film.taking_filter = v,
            "--amount" => s.film.taking_filter_amount = v.parse()?,
            "--iso" => {
                let iso: f64 = v.parse()?;
                let session = Session::open(file)?;
                let mut cal = (*session.cal).clone();
                cal.iso = iso;
                print_weights(&cal, &s);
                return Ok(());
            }
            a => bail!("unknown option {a}"),
        }
        i += 2;
    }
    let session = Session::open(file)?;
    print_weights(&session.cal, &s);
    Ok(())
}

fn print_weights(cal: &develop::Calibration, s: &Settings) {
    match develop::analog::weights(cal, s) {
        Some(w) => println!(
            "{} ISO {}: weights bottom/middle/top {:.3} {:.3} {:.3} · noise (rel. at middle grey) {:.4} · today {:.4}",
            s.film.stock, cal.iso, w.w[0], w.w[1], w.w[2], w.noise, develop::analog::today_noise(cal, s).unwrap_or(f32::NAN)
        ),
        None => println!("not available for {} / {}", cal.camera, s.film.stock),
    }
}
