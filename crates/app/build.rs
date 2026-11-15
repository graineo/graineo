//! Windows: embed the app symbol (`assets/graineo.ico`) into the program file, so the Explorer and the
//! taskbar show it. Needs the resource compiler of MinGW (`x86_64-w64-mingw32-windres`, comes with
//! `brew install mingw-w64`, see scripts/bundle-windows.sh); without it the program builds without a symbol.
//! Other systems: nothing to do (macOS takes the symbol from the app bundle).

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/graineo.ico");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let ico = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir")).join("assets/graineo.ico");
    let rc = out.join("graineo.rc");
    let obj = out.join("graineo-icon.o");
    // forward slashes: windres reads backslashes as escapes
    std::fs::write(&rc, format!("1 ICON \"{}\"\n", ico.display().to_string().replace('\\', "/"))).expect("write .rc");
    let windres = std::env::var("WINDRES").unwrap_or_else(|_| "x86_64-w64-mingw32-windres".into());
    match Command::new(&windres).arg(&rc).arg("-O").arg("coff").arg("-o").arg(&obj).status() {
        Ok(s) if s.success() => println!("cargo:rustc-link-arg-bins={}", obj.display()),
        _ => println!("cargo:warning=windres not found or failed: program file without symbol"),
    }
}
