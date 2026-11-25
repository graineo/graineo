#!/bin/bash
# Follow-up step of a release for the Linux packages (run locally, after scripts/release.sh and the upload of the
# release). CI cannot sign: the update signing key never leaves this machine. The Linux packages only exist once the
# workflow "Linux (experimental)" has attached them to the release, so the signed update list gets their entries here:
#
#   1. gh workflow run linux.yml -f release_tag=v<version>     (builds and attaches the two .tar.gz and
#      SHA256SUMS-linux-experimental; wait until it has finished)
#   2. scripts/linux-sign.sh [<version>]                        downloads the packages and the published latest.json,
#      checks them, writes latest.json with the Linux entries, signs it and verifies the signature
#   3. UPLOAD=1 scripts/linux-sign.sh [<version>]                 the same and then replaces latest.json, latest.json.sig
#      and SHA256SUMS.txt of the release (gh release upload --clobber)
#
# Without UPLOAD=1 nothing is uploaded; the upload command is printed instead. Until step 3, installed Linux builds
# treat the release as not yet available for them (no package entry); macOS and Windows are not affected.
# What is signed: size and SHA-256 of the packages as downloaded here. The script checks before signing that they match
# the checksum list of the workflow, that each package carries the release version and that its program is built for
# the processor of its name.
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/app-identity.sh
VERSION=${1:-$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)}
TAG="v$VERSION"
T=${CARGO_TARGET_DIR:-target}
DEV="${GRAINEO_DEV:-$T/release/graineo-dev}"
KEY="$HOME/.graineo-signing/update-ed25519.key"
OUT="${RELEASE_DIR:-dist/release-$VERSION}/linux"
[[ -x "$DEV" ]] || { echo "missing: $DEV (cargo build --release -p graineo-dev)" >&2; exit 1; }
[[ -f "$KEY" ]] || { echo "missing: update signing key $KEY" >&2; exit 1; }
command -v gh > /dev/null || { echo "missing: gh (GitHub CLI)" >&2; exit 1; }
REPO=$(grep -A2 'macro_rules! repo_url' crates/library/src/update.rs | grep -o '"https://[^"]*"' | tr -d '"')
[[ -n "$REPO" ]] || { echo "repository address not found in crates/library/src/update.rs" >&2; exit 1; }
SLUG=${REPO#https://github.com/}
BASE="$REPO/releases/download/$TAG"
SUMS=SHA256SUMS-linux-experimental

rm -rf "$OUT"
mkdir -p "$OUT/published"
# packages and checksum list of the workflow, the published (signed) list and the checksums of the release
gh release download "$TAG" --repo "$SLUG" --dir "$OUT" --pattern "*-linux-*-experimental.tar.gz" --pattern "$SUMS" || {
  echo "Linux packages not attached to $TAG yet: gh workflow run linux.yml --repo $SLUG -f release_tag=$TAG" >&2
  exit 1
}
gh release download "$TAG" --repo "$SLUG" --dir "$OUT/published" --pattern latest.json --pattern latest.json.sig --pattern SHA256SUMS.txt

# 1. consistency with the workflow's checksum list
(cd "$OUT" && shasum -a 256 -c "$SUMS")

# 2. each package: release version and processor
ASSETS=()
for ARCH in x86_64 aarch64; do
  PKG="$APP_NAME-$VERSION-linux-$ARCH-experimental.tar.gz"
  [[ -f "$OUT/$PKG" ]] || { echo "missing package: $PKG" >&2; exit 1; }
  CHECK=$(mktemp -d)
  tar -xzf "$OUT/$PKG" -C "$CHECK" "$APP_NAME/README-linux.txt" "$APP_NAME/$APP_NAME" "$APP_NAME/install.sh"
  head -n 1 "$CHECK/$APP_NAME/README-linux.txt" | grep -q "^$APP_NAME $VERSION," || { echo "$PKG: version is not $VERSION" >&2; exit 1; }
  case $ARCH in
    x86_64) WANT="x86-64" ;;
    aarch64) WANT="aarch64" ;;
  esac
  file "$CHECK/$APP_NAME/$APP_NAME" | grep -q "ELF 64-bit.*$WANT" || { echo "$PKG: program is not built for $ARCH" >&2; exit 1; }
  rm -rf "$CHECK"
  ASSETS+=(--asset "linux-$ARCH" "$OUT/$PKG" "$BASE/$PKG")
done

# 3. new list: the published one (signature checked by graineo-dev) plus the Linux entries, signed and verified
"$DEV" update-manifest "$OUT/latest.json" --version "$VERSION" --base "$OUT/published/latest.json" "${ASSETS[@]}"
"$DEV" update-sign "$OUT/latest.json" --key "$KEY"
"$DEV" update-verify "$OUT/latest.json"

# 4. checksums of the release: replace the lines of the list and its signature
{
  grep -v -E '  latest\.json(\.sig)?$' "$OUT/published/SHA256SUMS.txt" || true
  (cd "$OUT" && shasum -a 256 latest.json latest.json.sig)
} > "$OUT/SHA256SUMS.txt"
cat "$OUT/SHA256SUMS.txt"

UPLOAD_CMD=(gh release upload "$TAG" --repo "$SLUG" --clobber "$OUT/latest.json" "$OUT/latest.json.sig" "$OUT/SHA256SUMS.txt")
if [[ "${UPLOAD:-0}" == 1 ]]; then
  "${UPLOAD_CMD[@]}"
  echo "uploaded: latest.json with Linux entries"
else
  echo "Not uploaded. To publish: UPLOAD=1 $0 $VERSION   (or: ${UPLOAD_CMD[*]})"
fi
