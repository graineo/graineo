//! Film simulation of a raw linear image (measurements, no photo files involved).
//!
//! `film_chart <in.f32> <width> <height> <settings.json> <out.u16> [cpu]`
//!
//! `in.f32`: scene-linear sRGB, little-endian f32, RGB interleaved. `settings.json`: the
//! `FilmSettings` fields to set (rest default, `active` is forced on). Output: sRGB-encoded
//! 16-bit RGB, little-endian. Film data via `GRAINEO_FILM_DATA` or the repository.
use develop::film::{self, FilmSettings};
use develop::look::Geo;
use develop::Linear;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 6 {
        eprintln!("usage: film_chart <in.f32> <w> <h> <settings.json> <out.u16> [cpu]");
        std::process::exit(2);
    }
    let (w, h): (usize, usize) = (a[2].parse().unwrap(), a[3].parse().unwrap());
    let bytes = std::fs::read(&a[1]).unwrap();
    let data: Vec<f32> = bytes.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
    assert_eq!(data.len(), w * h * 3);
    let mut fs: FilmSettings = serde_json::from_str(&std::fs::read_to_string(&a[4]).unwrap()).unwrap();
    fs.active = true;
    if a.get(6).is_some_and(|b| b == "cpu") {
        film::set_backend(film::Backend::Cpu);
    }
    let scene = Linear { width: w, height: h, data, detail: None, haze: None, toneq: None, mono: false };
    let out = film::develop(&scene, &fs, Geo::full(w, h)).unwrap();
    let bytes: Vec<u8> = out.data.iter().flat_map(|v| v.to_le_bytes()).collect();
    std::fs::write(&a[5], bytes).unwrap();
}
