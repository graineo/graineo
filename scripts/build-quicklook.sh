#!/bin/bash
# Builds the two Quick Look extensions for X3F and places them in an app bundle:
#   Contents/PlugIns/GraineoQuickLookPreview.appex    space bar in the Finder (embedded camera JPEG)
#   Contents/PlugIns/GraineoQuickLookThumbnail.appex  Finder thumbnails
# Usage: scripts/build-quicklook.sh <path/to/graineo.app> [rust target, e.g. x86_64-apple-darwin]
# Only needs the Command Line Tools (swiftc), no Xcode project. The extensions are signed here
# (sandbox entitlements); the app itself is signed afterwards by bundle-macos.sh, without --deep,
# so the entitlements of the extensions stay intact.
#
# Signing and notarisation (for passing the app on): the extensions need the same identity as the
# app ("Developer ID Application: …" with SIGN_IDENTITY) and the hardened runtime; then notarise the
# whole app as usual (`xcrun notarytool submit … --wait`, `xcrun stapler staple`). The entitlements
# (QuickLook.entitlements) need no change for that. For the own Mac the local signature is enough.
set -euo pipefail
cd "$(dirname "$0")/.."
# BUNDLE_ID from the caller (bundle-macos.sh) wins over the default of scripts/bundle-id.txt
GIVEN_BUNDLE_ID="${BUNDLE_ID:-}"
source scripts/app-identity.sh
BUNDLE_ID="${GIVEN_BUNDLE_ID:-$BUNDLE_ID}"
APP="$1"
TARGET="${2:-}"
case "$TARGET" in
  x86_64-apple-darwin) ARCH=x86_64 ;;
  ""|aarch64-apple-darwin) ARCH=arm64 ;;
  *) echo "unknown target $TARGET" >&2; exit 1 ;;
esac
SRC=macos/quicklook
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
IDENT="$(sign_identity)"
UTI=com.sigmaphoto.photopro-x3f
mkdir -p "$APP/Contents/PlugIns"

# $1 name, $2 bundle id suffix, $3 extension point, $4 principal class, $5 framework, $6 source, $7 extra attributes
build_appex() {
  local NAME="$1" BID="$BUNDLE_ID.$2" POINT="$3" CLASS="$4" FW="$5" MAIN="$6" ATTR="$7"
  local EX="$APP/Contents/PlugIns/$NAME.appex"
  rm -rf "$EX"
  mkdir -p "$EX/Contents/MacOS"
  swiftc -O -whole-module-optimization -parse-as-library -application-extension \
    -target "$ARCH-apple-macos12.0" -module-name "$NAME" \
    -framework "$FW" -Xlinker -e -Xlinker _NSExtensionMain \
    -o "$EX/Contents/MacOS/$NAME" "$SRC/X3FQuickLook.swift" "$SRC/$MAIN"
  cat > "$EX/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>${BID}</string>
  <key>CFBundleName</key><string>${NAME}</string>
  <key>CFBundleDisplayName</key><string>graineo X3F</string>
  <key>CFBundleExecutable</key><string>${NAME}</string>
  <key>CFBundlePackageType</key><string>XPC!</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSExtension</key>
  <dict>
    <key>NSExtensionPointIdentifier</key><string>${POINT}</string>
    <key>NSExtensionPrincipalClass</key><string>${CLASS}</string>
    <key>NSExtensionAttributes</key>
    <dict>
      <key>QLSupportedContentTypes</key><array><string>${UTI}</string></array>
${ATTR}
    </dict>
  </dict>
</dict>
</plist>
PLIST
  plutil -lint -s "$EX/Contents/Info.plist"
  if security find-identity -p codesigning 2>/dev/null | grep -qF "$IDENT" \
     && codesign --force $(codesign_extra "$IDENT") --entitlements "$SRC/QuickLook.entitlements" --sign "$IDENT" "$EX" >/dev/null 2>&1; then
    :
  else
    codesign --force --entitlements "$SRC/QuickLook.entitlements" --sign - "$EX" >/dev/null
  fi
}

build_appex GraineoQuickLookPreview quicklook-preview com.apple.quicklook.preview PreviewProvider QuickLookUI PreviewProvider.swift \
  '      <key>QLIsDataBasedPreview</key><true/>
      <key>QLSupportsSearchableItems</key><false/>'
build_appex GraineoQuickLookThumbnail quicklook-thumbnail com.apple.quicklook.thumbnail ThumbnailProvider QuickLookThumbnailing ThumbnailProvider.swift \
  '      <key>QLThumbnailMinimumDimension</key><integer>0</integer>'
echo "Quick Look extensions: $APP/Contents/PlugIns"
