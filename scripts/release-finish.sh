#!/bin/bash
# Packing stage of scripts/release.sh, also on its own (e.g. notarising after the build, without a new build):
#   NOTARIZE=1 SIGN_IDENTITY="Developer ID Application: …" scripts/release-finish.sh
# Takes dist/release-<version>/graineo.app (or RELEASE_DIR/graineo.app) (signed by release.sh) and writes next to it:
#   graineo-<version>-macOS.zip, graineo-<version>-macOS.dmg, latest.json(.sig), SHA256SUMS.txt
# NOTARIZE=1: notarise the app and staple the ticket, then the DMG as well (profile NOTARY_PROFILE in
# scripts/app-identity.sh, set up once with `xcrun notarytool store-credentials`).
# The Windows ZIP must already be in the folder. Download addresses come from the repository address in
# crates/library/src/update.rs (repo_url!), the one place it is written down.
# Uploads nothing.
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/app-identity.sh
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
T=${CARGO_TARGET_DIR:-target}
OUT="${RELEASE_DIR:-dist/release-$VERSION}"
APP="$OUT/$APP_NAME.app"
IDENT="${SIGN_IDENTITY:--}"
KEY="$HOME/.graineo-signing/update-ed25519.key"
DEV="${GRAINEO_DEV:-$T/release/graineo-dev}"
WIN="$OUT/$APP_NAME-$VERSION-windows-x64.zip"
[[ -d "$APP" ]] || { echo "missing: $APP (scripts/release.sh)" >&2; exit 1; }
[[ -f "$WIN" ]] || { echo "missing: $WIN (scripts/release.sh)" >&2; exit 1; }
[[ -x "$DEV" ]] || { echo "missing: $DEV (cargo build --release -p graineo-dev)" >&2; exit 1; }
REPO=$(grep -A2 'macro_rules! repo_url' crates/library/src/update.rs | grep -o '"https://[^"]*"' | tr -d '"')
[[ -n "$REPO" ]] || { echo "repository address not found in crates/library/src/update.rs" >&2; exit 1; }
BASE="$REPO/releases/download/v$VERSION"
codesign --verify --deep --strict "$APP"

# 1. Notarisation of the app (ZIP upload), ticket stapled to the app
if [[ "${NOTARIZE:-0}" == 1 ]]; then
  [[ -n "$(codesign_extra "$IDENT")" ]] || { echo "NOTARIZE=1 needs SIGN_IDENTITY=\"Developer ID Application: …\"" >&2; exit 1; }
  xcrun notarytool history --keychain-profile "$NOTARY_PROFILE" >/dev/null
  ditto -c -k --keepParent "$APP" "$OUT/notarize.zip"
  xcrun notarytool submit "$OUT/notarize.zip" --keychain-profile "$NOTARY_PROFILE" --wait
  rm -f "$OUT/notarize.zip"
  xcrun stapler staple "$APP"
  xcrun stapler validate "$APP"
  spctl --assess --type execute --verbose=2 "$APP"
fi

# 2. ZIP of the app for the in-app update (keeps the signature, the ticket and attributes)
rm -f "$OUT/$APP_NAME-$VERSION-macOS.zip" "$OUT/$APP_NAME-$VERSION-macOS.dmg"
ditto -c -k --keepParent "$APP" "$OUT/$APP_NAME-$VERSION-macOS.zip"

# 3. DMG: the app plus a link to /Applications for drag-and-drop installation; signed (and notarised)
#    with a Developer ID
STAGE=$(mktemp -d)
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
hdiutil create -quiet -volname "$APP_NAME $VERSION" -srcfolder "$STAGE" -fs HFS+ -format UDZO -ov "$OUT/$APP_NAME-$VERSION-macOS.dmg"
rm -rf "$STAGE"
DMG="$OUT/$APP_NAME-$VERSION-macOS.dmg"
if [[ -n "$(codesign_extra "$IDENT")" ]]; then
  codesign --force --timestamp --sign "$IDENT" "$DMG"
  codesign --verify --strict "$DMG"
  if [[ "${NOTARIZE:-0}" == 1 ]]; then
    xcrun notarytool submit "$DMG" --keychain-profile "$NOTARY_PROFILE" --wait
    xcrun stapler staple "$DMG"
    xcrun stapler validate "$DMG"
    spctl --assess --type open --context context:primary-signature --verbose=2 "$DMG"
  fi
fi

# 4. Update manifest, signed with the release key; the app checks it with its built-in public key
"$DEV" update-manifest "$OUT/latest.json" --version "$VERSION" --notes "$OUT/NOTES.md" --notes-de "$OUT/NOTES.de.md" \
  --asset macos-universal "$OUT/$APP_NAME-$VERSION-macOS.zip" "$BASE/$APP_NAME-$VERSION-macOS.zip" \
  --asset windows-x64 "$WIN" "$BASE/$APP_NAME-$VERSION-windows-x64.zip"
rm -f "$OUT/latest.json.sig"
if [[ -f "$KEY" ]]; then
  "$DEV" update-sign "$OUT/latest.json" --key "$KEY"
  "$DEV" update-verify "$OUT/latest.json"
else
  echo "Note: update signing key missing ($KEY) – latest.json is NOT signed (graineo-dev update-key)." >&2
fi

# 5. Checksums
(cd "$OUT" && shasum -a 256 $APP_NAME-* latest.json $( [[ -f latest.json.sig ]] && echo latest.json.sig ) > SHA256SUMS.txt)
ls -la "$OUT"
cat "$OUT/SHA256SUMS.txt"
