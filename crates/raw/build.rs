//! Bundles every camera look found in `looks/<camera>/<style>.x3look` (sorted by name). The list of
//! bundled looks is therefore exactly the set of files in that folder; nothing has to be registered twice.
//! Personal looks never live here: they are files in the user's data folder (`rawdev::looks`).

use std::fmt::Write as _;
use std::path::Path;

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("looks");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut names = Vec::new();
    if let Ok(cams) = std::fs::read_dir(&dir) {
        for cam in cams.flatten() {
            println!("cargo:rerun-if-changed={}", cam.path().display());
            let Ok(styles) = std::fs::read_dir(cam.path()) else { continue };
            for f in styles.flatten() {
                let p = f.path();
                if p.extension().and_then(|e| e.to_str()) == Some("x3look") {
                    let style = p.file_stem().unwrap().to_string_lossy().into_owned();
                    names.push(format!("{}/{}", cam.file_name().to_string_lossy(), style));
                }
            }
        }
    }
    names.sort();
    let mut out = String::from("&[\n");
    for n in &names {
        let _ = writeln!(out, "    ({n:?}, include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/looks/{n}.x3look\"))),");
    }
    out.push(']');
    let target = Path::new(&std::env::var("OUT_DIR").unwrap()).join("looks.rs");
    std::fs::write(target, out).unwrap();
}
