#!/bin/bash
# Build the Windows version (64-bit) on the Mac: a single graineo.exe that needs only system components
# on Windows 10/11. Requirements: `rustup target add x86_64-pc-windows-gnu`, `brew install mingw-w64`.
# Result: dist/graineo-<version>-windows-x64.zip
set -euo pipefail
# NO_SYMBOLS=1: package without symbols (no debug info, stripped: no function names or source paths of the
# program in the package), for packages that are passed on
if [ "${NO_SYMBOLS:-0}" = "1" ]; then
  export CARGO_PROFILE_RELEASE_DEBUG=0 CARGO_PROFILE_RELEASE_STRIP=symbols
fi
cd "$(dirname "$0")/.."
# Source paths of this machine (panic places of all libraries, debug info) contain the user name: map the home
# folder to "~" and the project to "graineo", so that no package shows them (once per target folder a full build).
case " ${RUSTFLAGS:-} " in *" --remap-path-prefix=$HOME=~ "*) ;; *) export RUSTFLAGS="--remap-path-prefix=$HOME=~ --remap-path-prefix=$PWD=graineo ${RUSTFLAGS:-}" ;; esac
source scripts/app-identity.sh
export PATH=~/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH
TARGET=x86_64-pc-windows-gnu
MINGW=$(brew --prefix mingw-w64)/toolchain-x86_64/x86_64-w64-mingw32
export BINDGEN_EXTRA_CLANG_ARGS_x86_64_pc_windows_gnu="--target=x86_64-w64-mingw32 -I$MINGW/include"
export CARGO_PROFILE_RELEASE_DEBUG=0 CARGO_PROFILE_RELEASE_STRIP=true
T=${CARGO_TARGET_DIR:-target}
cargo build --release -j 4 --target $TARGET -p graineo
VER=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
TMP=$(mktemp -d)
mkdir -p "$TMP/graineo" dist
cp "$T/$TARGET/release/graineo.exe" "$TMP/graineo/graineo.exe"
cp LICENSE "$TMP/graineo/" 2>/dev/null || true
# Licence texts of the bundled components, as in the macOS package (Contents/Resources/licenses)
mkdir -p "$TMP/graineo/licenses"
cp LICENSE THIRD_PARTY.md "$TMP/graineo/licenses/"
[ -d LICENSES ] && cp LICENSES/* "$TMP/graineo/licenses/"
cp third_party/x3fuse-core/LICENSE "$TMP/graineo/licenses/Apache-2.0-x3fuse-core.txt"
cp third_party/x3fuse-core/NOTICE "$TMP/graineo/licenses/NOTICE-x3fuse-core.txt"
cp third_party/x3fuse-core/MODIFICATIONS.md "$TMP/graineo/licenses/MODIFICATIONS-x3fuse-core.md"
cp third_party/egui-wgpu/LICENSE-MIT "$TMP/graineo/licenses/MIT-egui-wgpu.txt"
cp crates/app/assets/fonts/OFL-*.txt crates/app/assets/fonts/LICENSE-Phosphor.txt "$TMP/graineo/licenses/"
# Film simulation: profiles (including the own B&W films), spectral LUTs, filters; develop::film looks for them
# next to the executable under spektrafilm/
mkdir -p "$TMP/graineo/spektrafilm"
cp -R third_party/spektrafilm-rs/data/profiles third_party/spektrafilm-rs/data/luts third_party/spektrafilm-rs/data/filters "$TMP/graineo/spektrafilm/"
(cd "$TMP" && zip -qr "graineo-$VER-windows-x64.zip" graineo)
mv "$TMP/graineo-$VER-windows-x64.zip" dist/
rm -rf "$TMP"
find "$T" -name '*.rcgu.o' -delete
echo "dist/graineo-$VER-windows-x64.zip"
