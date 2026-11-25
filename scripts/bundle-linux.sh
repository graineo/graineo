#!/bin/bash
# Build the EXPERIMENTAL Linux version on the Mac (in a container, nothing is installed on the Mac besides
# docker + colima): tar.gz with the program, desktop file, icon, film data and licences.
# Needs: docker, colima (`colima start --cpu 6 --memory 8 --disk 60`).
# ARCH=aarch64 (default, native on Apple Silicon) or x86_64 (emulated by colima, very slow: use --arch x86_64
# for a separate colima profile).
# NATIVE=1: no Docker, use the cargo build of this machine (CI runner, ARCH from `uname -m`); SKIP_BUILD=1 then
# only packages an existing `cargo build --release -p graineo -p graineo-dev`.
# Test packages for the update test (never for a release): GRAINEO_UPDATE_TEST_PUBKEY_BUILD=<base64 public key> compiles
# a test key in, which the app trusts only in a test run (GRAINEO_DATA_DIR and GRAINEO_UPDATE_URL); TARGET_VOLUME
# picks another build volume, CARGO_PROFILE_RELEASE_LTO / _CODEGEN_UNITS are passed on for faster test builds.
# Result: dist/graineo-<version>-linux-<arch>-experimental.tar.gz plus dist/SHA256SUMS
set -euo pipefail
cd "$(dirname "$0")/.."
if [ "${NATIVE:-0}" = 1 ]; then
  ARCH=${ARCH:-$(uname -m)}
  [ "$ARCH" = arm64 ] && ARCH=aarch64
else
  ARCH=${ARCH:-aarch64}
fi
VER=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
TMP=$(mktemp -d)
D="$TMP/graineo"
mkdir -p "$D/licenses" dist
if [ "${NATIVE:-0}" = 1 ]; then
  BIN=${CARGO_TARGET_DIR:-target}/release
  if [ "${SKIP_BUILD:-0}" != 1 ]; then
    CARGO_PROFILE_RELEASE_DEBUG=0 CARGO_PROFILE_RELEASE_STRIP=true \
      RUSTFLAGS="--remap-path-prefix=$PWD=graineo --remap-path-prefix=$HOME/.cargo=cargo" \
      cargo build --release --locked -p graineo -p graineo-dev
  fi
  cp "$BIN/graineo" "$BIN/graineo-dev" "$D/"
else
  IMAGE=graineo-linux-build
  VOLUME=${TARGET_VOLUME:-graineo-linux-target-$ARCH}
  EXTRA=()
  for v in GRAINEO_UPDATE_TEST_PUBKEY_BUILD CARGO_PROFILE_RELEASE_LTO CARGO_PROFILE_RELEASE_CODEGEN_UNITS; do
    [ -n "${!v:-}" ] && EXTRA+=(-e "$v=${!v}")
  done
  [ -n "${GRAINEO_UPDATE_TEST_PUBKEY_BUILD:-}" ] && echo "WARNING: test package with a compiled-in update test key, never publish it" >&2
  docker build -q -t $IMAGE scripts/linux > /dev/null
  docker volume create "$VOLUME" > /dev/null
  docker volume create graineo-linux-cargo > /dev/null
  # Source paths of this machine in panic messages: map the project to "graineo"
  RUST_REMAP="--remap-path-prefix=/src=graineo --remap-path-prefix=/root/.cargo=cargo"
  docker run --rm --cpus "${CPUS:-5}" -v "$PWD":/src:ro -v "$VOLUME":/target -v graineo-linux-cargo:/root/.cargo/registry \
    -e CARGO_TARGET_DIR=/target -e CARGO_PROFILE_RELEASE_DEBUG=0 -e CARGO_PROFILE_RELEASE_STRIP=true \
    -e RUSTFLAGS="$RUST_REMAP" ${EXTRA[@]+"${EXTRA[@]}"} -w /src $IMAGE \
    cargo build --release -j "${CPUS:-5}" -p graineo -p graineo-dev
  # (the temporary folder of the Mac is not shared with the VM: copy out of a container instead of a bind mount)
  CID=$(docker create -v "$VOLUME":/target $IMAGE)
  docker cp "$CID:/target/release/graineo" "$D/graineo"
  docker cp "$CID:/target/release/graineo-dev" "$D/graineo-dev"
  docker rm "$CID" > /dev/null
fi
cp crates/app/assets/icon-1024.png "$D/graineo.png"
cp scripts/linux-install.sh "$D/install.sh" && chmod +x "$D/install.sh"
cat > "$D/graineo.desktop" <<DESK
[Desktop Entry]
Type=Application
Name=graineo
Comment=RAW & Film, with Foveon support (experimental Linux build)
Exec=graineo %F
Icon=graineo
Categories=Graphics;Photography;
MimeType=image/jpeg;image/png;image/tiff;image/x-sigma-x3f;image/x-canon-cr2;image/x-canon-cr3;image/x-fuji-raf;image/x-adobe-dng;image/x-sony-arw;
Terminal=false
DESK
cat > "$D/README-linux.txt" <<README
graineo $VER, EXPERIMENTAL Linux build ($ARCH). Not tested on real hardware. Updates install themselves when the program folder is writable (as with install.sh).
Face detection and Apple Photos are not available. Needs a Vulkan driver (Mesa: mesa-vulkan-drivers; with
lavapipe also without a GPU, slow), libxkbcommon, fontconfig and X11 or Wayland.
Install: ./install.sh   (program and film data to ~/.local/share/graineo/app, command ~/.local/bin/graineo,
entry in the application menu). Remove: ~/.local/share/graineo/app/install.sh --uninstall (keeps your library).
Or start without installing: ./graineo   (keep the folder together; the film data are also built into the program)
Data: ~/.local/share/graineo, cache: ~/.cache/graineo, log: ~/.local/state/graineo/logs
README
cp LICENSE THIRD_PARTY.md "$D/licenses/"
[ -d LICENSES ] && cp LICENSES/* "$D/licenses/"
cp LICENSE "$D/"
cp third_party/x3fuse-core/LICENSE "$D/licenses/Apache-2.0-x3fuse-core.txt"
cp third_party/x3fuse-core/NOTICE "$D/licenses/NOTICE-x3fuse-core.txt"
cp third_party/x3fuse-core/MODIFICATIONS.md "$D/licenses/MODIFICATIONS-x3fuse-core.md"
cp third_party/egui-wgpu/LICENSE-MIT "$D/licenses/MIT-egui-wgpu.txt"
cp crates/app/assets/fonts/OFL-*.txt crates/app/assets/fonts/LICENSE-Phosphor.txt "$D/licenses/"
# Film simulation data next to the program (develop::film looks for spektrafilm/)
mkdir -p "$D/spektrafilm"
cp -R third_party/spektrafilm-rs/data/profiles third_party/spektrafilm-rs/data/luts third_party/spektrafilm-rs/data/filters "$D/spektrafilm/"
# macOS tar (bsdtar) would add extended attributes (com.apple.provenance) that GNU tar reports on every file
TAR_OPTS=()
tar --version 2> /dev/null | grep -q bsdtar && TAR_OPTS=(--no-xattrs --no-mac-metadata)
COPYFILE_DISABLE=1 tar ${TAR_OPTS[@]+"${TAR_OPTS[@]}"} -C "$TMP" -czf "dist/graineo-$VER-linux-$ARCH-experimental.tar.gz" graineo
rm -rf "$TMP"
(cd dist && if command -v sha256sum > /dev/null; then sha256sum "graineo-$VER-linux-$ARCH-experimental.tar.gz"; else shasum -a 256 "graineo-$VER-linux-$ARCH-experimental.tar.gz"; fi >> SHA256SUMS.new)
# keep lines of other architectures, replace the one of this package
{ grep -v " graineo-$VER-linux-$ARCH-experimental" dist/SHA256SUMS 2> /dev/null || true; cat dist/SHA256SUMS.new; } > dist/SHA256SUMS.tmp
mv dist/SHA256SUMS.tmp dist/SHA256SUMS; rm -f dist/SHA256SUMS.new
echo "dist/graineo-$VER-linux-$ARCH-experimental.tar.gz"
