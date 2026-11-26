//! `heic_probe <file>…` (`cargo run -p library --example heic_probe`): HEIC/HEIF decoding with the built-in decoder,
//! per file: the size and orientation the library reports, decode times at full size and of the embedded thumbnail,
//! and (outside macOS) the app's path (`imageio::thumb`, `imageio::image`) with the colour of a pixel.
//! Three rounds each, the fastest time is printed. Run with `GRAINEO_DATA_DIR` set to a scratch folder.

use std::path::Path;
use std::time::{Duration, Instant};

type Result<T> = std::result::Result<T, String>;

fn best<T>(mut f: impl FnMut() -> Option<T>) -> (Option<T>, Duration) {
    let mut r = None;
    let mut t = Duration::MAX;
    for _ in 0..3 {
        let s = Instant::now();
        r = f();
        t = t.min(s.elapsed());
    }
    (r, t)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    for a in &args {
        let p = Path::new(a);
        println!("== {}", p.display());
        let e = library::imageio::properties(p);
        println!("properties: {:?}", e.as_ref().map(|e| (e.width, e.height, e.orientation)));
        let (r, t) = best(|| library::heif_lib::linear(p).ok().map(|r| (r.0, r.1)));
        println!("full: {r:?} {:.0} ms", t.as_secs_f64() * 1e3);
        if r.is_none() {
            return Err(format!("{}: not decoded: {:?}", p.display(), library::heif_lib::linear(p).err()));
        }
        let (r, t) = best(|| library::heif_lib::thumbnail(p).map(|r| (r.0, r.1)));
        println!("embedded thumbnail: {r:?} {:.1} ms", t.as_secs_f64() * 1e3);
        let (r, t) = best(|| library::imageio::thumb(p, 256, false).map(|r| (r.0, r.1)));
        println!("imageio::thumb 256: {r:?} {:.0} ms", t.as_secs_f64() * 1e3);
        let (r, t) = best(|| library::imageio::image(p, None));
        let Some((w, h, px)) = &r else { return Err(format!("{}: imageio::image failed", p.display())) };
        let i = ((*h as usize / 4) * *w as usize + *w as usize / 2) * 3;
        println!("imageio::image: {w}×{h} {:.0} ms, pixel (w/2, h/4) = {:?}", t.as_secs_f64() * 1e3, &px[i..i + 3]);
    }
    Ok(())
}
