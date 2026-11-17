#!/bin/bash
# Builds the release packages for GitHub Releases under dist/release-<version>/:
#   graineo-<version>-macOS.dmg        universal app (Apple silicon + Intel), macOS 12 or later
#   graineo-<version>-windows-x64.zip  Windows 10/11, 64-bit
#   graineo-<version>-macOS.zip        the same universal app for the in-app update (ditto)
#   latest.json, latest.json.sig        update manifest (download addresses, sizes, SHA-256) and its
#                                       Ed25519 signature (key: ~/.graineo-signing/update-ed25519.key)
#   SHA256SUMS.txt                      checksums of all of them
#   graineo.app                         the signed app the packages are made from (not uploaded)
# Release notes for latest.json: dist/release-<version>/NOTES.md (English) and NOTES.de.md (German),
# both optional; they are kept when the folder is rebuilt.
#
# Signature: SIGN_IDENTITY="Developer ID Application: …" signs app, Quick Look extensions and DMG with the
# hardened runtime and a timestamp (needed for notarisation); without it the app is signed ad hoc.
# Notarisation: NOTARIZE=1 (profile NOTARY_PROFILE, scripts/app-identity.sh). The packing stage is a script of
# its own (scripts/release-finish.sh), so notarising later needs no new build:
#   NOTARIZE=1 SIGN_IDENTITY="Developer ID Application: …" scripts/release-finish.sh
# Uploads nothing. The app installed on this Mac is not touched.
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/app-identity.sh
export PATH=~/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH
export MACOSX_DEPLOYMENT_TARGET=12.0
# packages for publication leave out the maintainer's personal camera looks (crates/raw/build.rs)
export GRAINEO_PUBLIC_BUILD=1
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
T=${CARGO_TARGET_DIR:-target}
# Source paths of this machine (panic places of all libraries, debug info) contain the user name: map the home
# folder to "~" and the project to "graineo", so that no package shows them (once per target folder a full build).
case " ${RUSTFLAGS:-} " in *" --remap-path-prefix=$HOME=~ "*) ;; *) export RUSTFLAGS="--remap-path-prefix=$HOME=~ --remap-path-prefix=$PWD=graineo ${RUSTFLAGS:-}" ;; esac
OUT="dist/release-$VERSION"
IDENT="${SIGN_IDENTITY:--}"
EXTRA=$(codesign_extra "$IDENT")
# keep the release notes written by hand
NOTES_TMP=$(mktemp -d)
for N in NOTES.md NOTES.de.md; do if [[ -f "$OUT/$N" ]]; then cp "$OUT/$N" "$NOTES_TMP/"; fi; done
rm -rf "$OUT"
mkdir -p "$OUT"
for N in NOTES.md NOTES.de.md; do if [[ -f "$NOTES_TMP/$N" ]]; then cp "$NOTES_TMP/$N" "$OUT/"; fi; done
rm -rf "$NOTES_TMP"

# 1. macOS: build both architectures (bundle-macos.sh with TARGET builds the .app without installing it)
for Z in aarch64-apple-darwin x86_64-apple-darwin; do
  TARGET=$Z SIGN_IDENTITY="$IDENT" ./scripts/bundle-macos.sh >/dev/null
  rm -f "dist/$APP_NAME-${VERSION}-${Z%%-*}.zip"
done
A64="$T/aarch64-apple-darwin/$APP_NAME.app"
X64="$T/x86_64-apple-darwin/$APP_NAME.app"
APP="$OUT/$APP_NAME.app"
cp -R "$A64" "$APP"
# universal: the program and both Quick Look extensions
lipo -create "$A64/Contents/MacOS/graineo" "$X64/Contents/MacOS/graineo" -output "$APP/Contents/MacOS/graineo"
strip -x "$APP/Contents/MacOS/graineo"
for EX in "$APP"/Contents/PlugIns/*.appex; do
  N=$(basename "$EX" .appex)
  lipo -create "$A64/Contents/PlugIns/$N.appex/Contents/MacOS/$N" "$X64/Contents/PlugIns/$N.appex/Contents/MacOS/$N" -output "$EX/Contents/MacOS/$N"
  codesign --force $EXTRA --entitlements macos/quicklook/QuickLook.entitlements --sign "$IDENT" "$EX"
done
# the app itself without --deep: that would sign the extensions again without their sandbox entitlements.
# Without a Developer ID: ad-hoc signature (required on Apple silicon); the first start then needs
# "Open Anyway" in System Settings → Privacy & Security.
ENT=()
[[ -n "$EXTRA" ]] && ENT=(--entitlements macos/graineo.entitlements)
codesign --force $EXTRA "${ENT[@]+"${ENT[@]}"}" --sign "$IDENT" "$APP"
lipo -info "$APP/Contents/MacOS/graineo"
for EX in "$APP"/Contents/PlugIns/*.appex; do lipo -info "$EX/Contents/MacOS/$(basename "$EX" .appex)"; done
codesign --verify --deep --strict --verbose=2 "$APP"
# minimum macOS 12 for every slice
if otool -l -arch all "$APP/Contents/MacOS/graineo" | awk '/minos/ {print $2}' | grep -qv '^12\.0$'; then echo "minimum macOS is not 12.0" >&2; exit 1; fi
# no path of the build machine in the program (see RUSTFLAGS above)
if strings "$APP/Contents/MacOS/graineo" | grep -qF "$HOME/"; then echo "home folder path in the macOS program" >&2; exit 1; fi

# 2. Windows (x64)
./scripts/bundle-windows.sh >/dev/null
mv "dist/$APP_NAME-$VERSION-windows-x64.zip" "$OUT/"
if unzip -p "$OUT/$APP_NAME-$VERSION-windows-x64.zip" '*.exe' | strings | grep -qF "$HOME/"; then echo "home folder path in the Windows program" >&2; exit 1; fi

# 3. graineo-dev for the update manifest (and the check script)
cargo build --release -j 4 -p graineo-dev
find "$T" -name '*.rcgu.o' -delete

# 4. notarise (NOTARIZE=1), pack ZIP and DMG, update manifest, checksums
SIGN_IDENTITY="$IDENT" ./scripts/release-finish.sh
