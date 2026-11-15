#!/bin/bash
# App name, identifier and signature in one place (sourced by bundle-macos.sh, build-quicklook.sh,
# bundle-windows.sh and release.sh). The bundle ID itself lives in scripts/bundle-id.txt; the program
# reads the same file (library::update::BUNDLE_ID), so the two never drift apart.
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_NAME="graineo"
BUNDLE_ID="$(tr -d '[:space:]' < "$HERE/bundle-id.txt")"
# Certificate for the fixed signature (keychain, type "Code Signing"). Until a certificate
# "graineo Signing" exists, the previous self-issued one is used, by its SHA-1 fingerprint
# (so the permissions granted in macOS are kept); SIGN_IDENTITY overrides both.
SIGN_IDENTITY_NEW="graineo Signing"
SIGN_IDENTITY_PREVIOUS="E1BBF19EDA36F48A49C38FAE9C1DF7481A596309"
# notarytool profile in the keychain (once: xcrun notarytool store-credentials "$NOTARY_PROFILE" …)
NOTARY_PROFILE="${NOTARY_PROFILE:-graineo}"

sign_identity() {
  if [[ -n "${SIGN_IDENTITY:-}" ]]; then echo "$SIGN_IDENTITY"
  elif security find-identity -p codesigning 2>/dev/null | grep -q "\"$SIGN_IDENTITY_NEW\""; then echo "$SIGN_IDENTITY_NEW"
  else echo "$SIGN_IDENTITY_PREVIOUS"; fi
}

# Extra codesign options: a Developer ID signature (passing the app on, notarisation) needs the hardened
# runtime and a secure timestamp; local signatures stay as they are.
codesign_extra() {
  case "$1" in "Developer ID Application"*) echo "--options runtime --timestamp" ;; esac
}
