//! Exports an X3F file with settings from a JSON file, for tests without UI.
//!
//! `cargo run --release -p develop --example export -- IMAGE.X3F settings.json out.(dng|tif|jpg) [profile|linear]`

use develop::export::{self, Engine, Format};
use develop::{Session, Settings};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("Usage: export IMAGE.X3F settings.json out.(dng|tif|jpg) [profile|linear]");
        std::process::exit(2);
    }
    let s: Settings = std::fs::read_to_string(&args[2]).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let out = std::path::Path::new(&args[3]);
    let format = match out.extension().and_then(|e| e.to_str()) {
        Some("dng") if args.get(4).map(String::as_str) == Some("linear") => Format::Dng,
        Some("dng") => Format::DngProfiles,
        Some("tif") | Some("tiff") => Format::Tiff16,
        _ => Format::Jpeg { quality: 92 },
    };
    let mut session = Session::open(&args[1]).expect("open file");
    let t = std::time::Instant::now();
    export::export_with(&mut session, &s, format, Engine::Spp, out).expect("Export");
    eprintln!("{} in {:.2?}", out.display(), t.elapsed());
}
