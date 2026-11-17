//! Bundles every camera look found in `looks/<camera>/<style>.x3look` (sorted by name). The list of
//! bundled looks is therefore exactly the set of files in that folder; nothing has to be registered twice.
//!
//! Packages made for publication (`GRAINEO_PUBLIC_BUILD=1`, set by `scripts/release.sh`) leave out the looks of the
//! maintainer's own cameras (style names starting with `user`): the public source does not contain them either, so
//! a package must not either.

use std::fmt::Write as _;
use std::path::Path;

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("looks");
    println!("cargo:rerun-if-changed={}", dir.display());
    println!("cargo:rerun-if-env-changed=GRAINEO_PUBLIC_BUILD");
    let public = std::env::var("GRAINEO_PUBLIC_BUILD").is_ok_and(|v| v == "1");
    let mut names = Vec::new();
    if let Ok(cams) = std::fs::read_dir(&dir) {
        for cam in cams.flatten() {
            println!("cargo:rerun-if-changed={}", cam.path().display());
            let Ok(styles) = std::fs::read_dir(cam.path()) else { continue };
            for f in styles.flatten() {
                let p = f.path();
                if p.extension().and_then(|e| e.to_str()) == Some("x3look") {
                    let style = p.file_stem().unwrap().to_string_lossy().into_owned();
                    if public && style.starts_with("user") {
                        continue;
                    }
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
