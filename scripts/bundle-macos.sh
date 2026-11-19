#!/bin/bash
# Builds "graineo.app" and installs it to ~/Applications.
# Name, bundle ID and signing identity: scripts/app-identity.sh and scripts/bundle-id.txt.
# With TARGET=x86_64-apple-darwin, the version for Intel Macs is built instead: not installed, but as a
# ZIP under dist/ (for copying to the other Mac). Requires at least macOS 12 (Monterey).
# With CHANNEL=performance (test build next to the real app): "graineo Test.app", bundle ID <bundle ID>.test,
# own data and cache folders (LSEnvironment), installed next to – never over – graineo.app. Version from Cargo.toml
# (branch version like 0.86.2-performance.N; no tag).
# With NO_INSTALL=1 the app is only built and packed as a ZIP under dist/ (nothing in ~/Applications changes).
# The bundle contains two Quick Look extensions for X3F (space bar and Finder thumbnails, see
# scripts/build-quicklook.sh and macos/quicklook/).
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
export PATH=/opt/homebrew/opt/rustup/bin:$PATH
export MACOSX_DEPLOYMENT_TARGET=12.0
TARGET="${TARGET:-}"
CHANNEL="${CHANNEL:-}"
APPNAME="$APP_NAME"
LSENV=""
if [[ -n "$CHANNEL" ]]; then
  [[ "$CHANNEL" == "performance" ]] || { echo "Unknown CHANNEL: $CHANNEL" >&2; exit 2; }
  [[ -z "$TARGET" ]] || { echo "CHANNEL and TARGET cannot be combined" >&2; exit 2; }
  APPNAME="$APP_NAME Test"
  BUNDLE_ID="$BUNDLE_ID.test"
  LSENV="
  <key>LSEnvironment</key>
  <dict>
    <key>GRAINEO_DATA_DIR</key><string>$HOME/Library/Application Support/$APP_NAME Test</string>
    <key>GRAINEO_CACHE_DIR</key><string>$HOME/Library/Caches/$APP_NAME Test/spp</string>
    <key>GRAINEO_LIBRARY_SIDECARS</key><string>0</string>
    <key>GRAINEO_NO_PHOTOKIT</key><string>1</string>
  </dict>"
fi
BUILD_DIR="${CARGO_TARGET_DIR:-target}"
if [[ -n "$TARGET" ]]; then
  cargo build --release -j "${JOBS:-4}" -p graineo --target "$TARGET"
  BIN="$BUILD_DIR/$TARGET/release/graineo"
  APP="$BUILD_DIR/$TARGET/$APP_NAME.app"
else
  cargo build --release -j "${JOBS:-4}" -p graineo
  BIN="$BUILD_DIR/release/graineo"
  APP="$BUILD_DIR/$APPNAME.app"
fi
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/graineo"
cp crates/app/assets/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"
# Film simulation: profiles, spectral LUTs and filter data from spektrafilm-rs (GPL-3.0, ≈ 24 MB);
# develop::film looks for them at runtime under Contents/Resources/spektrafilm
mkdir -p "$APP/Contents/Resources/spektrafilm"
cp -R third_party/spektrafilm-rs/data/profiles third_party/spektrafilm-rs/data/luts third_party/spektrafilm-rs/data/filters "$APP/Contents/Resources/spektrafilm/"
# Licence texts of graineo and the bundled components (GPL-3.0, LGPL-2.1 of rawler, Apache-2.0 of x3fuse-core, OFL, MIT)
mkdir -p "$APP/Contents/Resources/licenses"
cp LICENSE THIRD_PARTY.md "$APP/Contents/Resources/licenses/"
[ -d LICENSES ] && cp LICENSES/* "$APP/Contents/Resources/licenses/"
cp third_party/x3fuse-core/LICENSE "$APP/Contents/Resources/licenses/Apache-2.0-x3fuse-core.txt"
cp third_party/x3fuse-core/NOTICE "$APP/Contents/Resources/licenses/NOTICE-x3fuse-core.txt"
cp third_party/x3fuse-core/MODIFICATIONS.md "$APP/Contents/Resources/licenses/MODIFICATIONS-x3fuse-core.md"
cp third_party/egui-wgpu/LICENSE-MIT "$APP/Contents/Resources/licenses/MIT-egui-wgpu.txt"
cp crates/app/assets/fonts/OFL-*.txt crates/app/assets/fonts/LICENSE-Phosphor.txt "$APP/Contents/Resources/licenses/"
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>${APPNAME}</string>
  <key>CFBundleDisplayName</key><string>${APPNAME}</string>
  <key>CFBundleIdentifier</key><string>${BUNDLE_ID}</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundleExecutable</key><string>graineo</string>${LSENV}
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleLocalizations</key><array><string>en</string><string>de</string><string>fr</string><string>es</string><string>ja</string></array>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSApplicationCategoryType</key><string>public.app-category.photography</string>
  <key>NSPhotoLibraryUsageDescription</key><string>graineo shows the photos of your Photos library, downloads originals from iCloud for editing and deletes photos only after you confirm.</string>
  <!-- import: memory cards (macOS asks once for "removable volumes"); mention cameras once the camera import is released -->
  <key>NSRemovableVolumesUsageDescription</key><string>graineo reads photos from memory cards to import them. Nothing on the card is changed.</string>
  <!-- Photos and folders can be dropped onto the Dock icon (opens the import); rank "Alternate": never the default app -->
  <key>CFBundleDocumentTypes</key>
  <array>
    <dict>
      <key>CFBundleTypeName</key><string>Photo</string>
      <key>CFBundleTypeRole</key><string>Viewer</string>
      <key>LSHandlerRank</key><string>Alternate</string>
      <key>LSItemContentTypes</key>
      <array><string>public.image</string><string>public.camera-raw-image</string><string>com.sigmaphoto.photopro-x3f</string><string>public.folder</string></array>
      <key>CFBundleTypeExtensions</key>
      <array><string>x3f</string><string>cr2</string><string>cr3</string><string>raf</string><string>dng</string><string>arw</string><string>nef</string><string>nrw</string><string>orf</string><string>ori</string><string>rw2</string><string>rwl</string><string>raw</string><string>pef</string><string>srw</string><string>crw</string><string>srf</string><string>sr2</string><string>mrw</string><string>erf</string><string>3fr</string><string>fff</string><string>iiq</string><string>mos</string><string>dcr</string><string>kdc</string><string>mef</string><string>heic</string><string>heif</string><string>hif</string><string>jpg</string><string>jpeg</string><string>tif</string><string>tiff</string><string>png</string></array>
    </dict>
  </array>
  <!-- X3F (Sigma Foveon RAW): macOS does not know the format. Imported under the identifier that SIGMA Photo Pro
       exports, so both apps name the same type; if SIGMA Photo Pro is installed, its declaration takes
       precedence. The Quick Look extensions in Contents/PlugIns are registered for this type. -->
  <key>UTImportedTypeDeclarations</key>
  <array>
    <dict>
      <key>UTTypeIdentifier</key><string>com.sigmaphoto.photopro-x3f</string>
      <key>UTTypeDescription</key><string>Sigma X3F RAW Image</string>
      <key>UTTypeConformsTo</key>
      <array><string>public.camera-raw-image</string></array>
      <key>UTTypeTagSpecification</key>
      <dict>
        <key>public.filename-extension</key>
        <array><string>x3f</string></array>
      </dict>
    </dict>
  </array>
  <!-- Preview store of the library (~/Library/Caches/graineo/graineo Previews.graineopreviews): a package, so
       the Finder shows it as a single file and nobody takes the previews for photos -->
  <key>UTExportedTypeDeclarations</key>
  <array>
    <dict>
      <key>UTTypeIdentifier</key><string>${BUNDLE_ID}.previews</string>
      <key>UTTypeDescription</key><string>graineo Previews</string>
      <key>UTTypeConformsTo</key>
      <array><string>com.apple.package</string><string>public.directory</string></array>
      <key>UTTypeTagSpecification</key>
      <dict>
        <key>public.filename-extension</key>
        <array><string>graineopreviews</string></array>
      </dict>
    </dict>
  </array>
</dict>
</plist>
PLIST
# German texts of the system dialogs (Apple Photos, removable volumes)
mkdir -p "$APP/Contents/Resources/de.lproj"
printf '"NSPhotoLibraryUsageDescription" = "graineo zeigt die Fotos deiner Bibliothek, lädt Originale aus iCloud zum Bearbeiten und löscht Fotos nur nach deiner Bestätigung.";\n' > "$APP/Contents/Resources/de.lproj/InfoPlist.strings"
printf '"NSRemovableVolumesUsageDescription" = "graineo liest Fotos von Speicherkarten, um sie zu importieren. Auf der Karte wird nichts verändert.";\n' >> "$APP/Contents/Resources/de.lproj/InfoPlist.strings"
# French, Spanish and Japanese texts of the same dialogs
for L in fr es ja; do mkdir -p "$APP/Contents/Resources/$L.lproj"; done
printf '"NSPhotoLibraryUsageDescription" = "graineo affiche les photos de votre photothèque, télécharge les originaux depuis iCloud pour les retoucher et ne supprime des photos qu’après votre confirmation.";\n' > "$APP/Contents/Resources/fr.lproj/InfoPlist.strings"
printf '"NSRemovableVolumesUsageDescription" = "graineo lit les photos des cartes mémoire pour les importer. Rien n’est modifié sur la carte.";\n' >> "$APP/Contents/Resources/fr.lproj/InfoPlist.strings"
printf '"NSPhotoLibraryUsageDescription" = "graineo muestra las fotos de tu mediateca, descarga los originales de iCloud para editarlos y solo elimina fotos tras tu confirmación.";\n' > "$APP/Contents/Resources/es.lproj/InfoPlist.strings"
printf '"NSRemovableVolumesUsageDescription" = "graineo lee las fotos de las tarjetas de memoria para importarlas. No se modifica nada en la tarjeta.";\n' >> "$APP/Contents/Resources/es.lproj/InfoPlist.strings"
printf '"NSPhotoLibraryUsageDescription" = "graineo は写真ライブラリの写真を表示し、編集のために iCloud からオリジナルをダウンロードします。写真の削除は確認後にのみ行います。";\n' > "$APP/Contents/Resources/ja.lproj/InfoPlist.strings"
printf '"NSRemovableVolumesUsageDescription" = "graineo はメモリーカードの写真を読み込みのために読み取ります。カード上のデータは変更されません。";\n' >> "$APP/Contents/Resources/ja.lproj/InfoPlist.strings"
# Quick Look extensions for X3F (signed there with their sandbox entitlements). Not in a test build (CHANNEL):
# its extensions would have the same bundle IDs as those of the real app.
[[ -n "$CHANNEL" ]] || BUNDLE_ID="$BUNDLE_ID" ./scripts/build-quicklook.sh "$APP" "$TARGET"
# Fixed signature: macOS only remembers permissions (e.g. "network volume" for the NAS) permanently if the app
# has the same identity on every installation. For that, create a self-signed certificate of type
# "Code Signing" with the name below once in the keychain (Keychain Access → Certificate Assistant). Without this
# certificate the app is signed ad hoc, and macOS asks again after every installation.
# Without --deep: the extensions are already signed, --deep would sign them again without their entitlements.
IDENT="$(sign_identity)"
# With a Developer ID additionally the hardened runtime, a timestamp and the entitlements (Photos library).
ENT=()
[[ -n "$(codesign_extra "$IDENT")" ]] && ENT=(--entitlements macos/graineo.entitlements)
if security find-identity -p codesigning 2>/dev/null | grep -qF "$IDENT" && codesign --force $(codesign_extra "$IDENT") "${ENT[@]+"${ENT[@]}"}" --sign "$IDENT" "$APP" >/dev/null 2>&1; then
  echo "Signed with: $IDENT"
else
  codesign --force --sign - "$APP" >/dev/null 2>&1
  echo "Note: certificate \"$IDENT\" not usable – signed ad hoc; macOS will ask again after every installation (network volume)."
fi
if [[ -n "$TARGET" || "${NO_INSTALL:-}" == 1 ]]; then
  mkdir -p dist
  ARCH_NAME="${TARGET:-aarch64-apple-darwin}"
  PACKAGE="dist/$APP_NAME-${VERSION}-${ARCH_NAME%%-*}.zip"
  rm -f "$PACKAGE"
  ditto -c -k --keepParent "$APP" "$PACKAGE"
  echo "Package: $PACKAGE"
  exit 0
fi
mkdir -p "$HOME/Applications"
DEST="$HOME/Applications/$APPNAME.app"
STAGE=$(mktemp -d "$HOME/Applications/.$APP_NAME-install.XXXXXX")
cp -R "$APP" "$STAGE/"
# The previous version is not kept: it waits in the staging folder until the new app is in place,
# then goes to the trash (never deleted for good). If the exchange fails, it is moved back.
OLD="$STAGE/previous"
mkdir -p "$OLD"
[[ -e "$DEST" ]] && mv "$DEST" "$OLD/$APPNAME.app"
if ! mv "$STAGE/$APPNAME.app" "$DEST"; then
  if [[ ! -e "$DEST" ]]; then
    [[ -e "$OLD/$APPNAME.app" ]] && mv "$OLD/$APPNAME.app" "$DEST"
  fi
  echo "Installation failed; the previous app was restored." >&2
  exit 1
fi
for A in "$OLD/$APPNAME.app"; do
  if [[ -e "$A" ]]; then
    if /usr/bin/trash "$A" 2>/dev/null; then echo "Previous app moved to the trash."
    else echo "Previous app could not be moved to the trash; it is in $A" >&2; fi
  fi
done
rmdir "$OLD" 2>/dev/null && rmdir "$STAGE" 2>/dev/null || true
