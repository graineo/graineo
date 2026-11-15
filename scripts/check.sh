#!/bin/bash
# Checks that the Sigma development (Merrill, Quattro, older Foveon cameras) stays bit-identical: develops the
# test files from the reference lists with graineo-dev and compares the checksums.
#
#   scripts/check.sh            after every change to crates/x3f, crates/develop or third_party/x3fuse-core
#
# References: crates/graineo-dev/reference/sigma.txt (columns: kind, file name, checksum), plus – if present –
# the local list $LOCAL/check-reference.lokal in the same format (test files that do not belong in the repository).
# Kinds: spphash = `graineo-dev spphash` (Sigma base); color/mono = image data (TIFF without header) from
# `graineo-dev develop <file> <out.tif> --spp --baseline [--mono]`.
#
# Where the test files are: GRAINEO_TEST_DATA (folders separated by ":") or one folder per line in
# $LOCAL/check-dirs.lokal; LOCAL is GRAINEO_CHECK_LOCAL or arbeitsordner/ in the project (git-ignored).
# Program: GRAINEO_DEV, otherwise ${CARGO_TARGET_DIR:-target}/release/graineo-dev (build it first).
# A missing file skips its line (reported, not an error). Exit code 1 on any mismatch.
# Runs with an empty data folder and without the disk cache; only reads the test files.
set -uo pipefail
cd "$(dirname "$0")/.."
ROOT=$PWD
LOCAL=${GRAINEO_CHECK_LOCAL:-$ROOT/arbeitsordner}
DEV=${GRAINEO_DEV:-${CARGO_TARGET_DIR:-$ROOT/target}/release/graineo-dev}
[ -x "$DEV" ] || { echo "graineo-dev missing ($DEV) – build it first: cargo build --release -p graineo-dev"; exit 2; }

REFS=("$ROOT/crates/graineo-dev/reference/sigma.txt")
[ -f "$LOCAL/check-reference.lokal" ] && REFS+=("$LOCAL/check-reference.lokal")

DIRS=()
if [ -n "${GRAINEO_TEST_DATA:-}" ]; then
  IFS=: read -r -a DIRS <<< "$GRAINEO_TEST_DATA"
elif [ -f "$LOCAL/check-dirs.lokal" ]; then
  while IFS= read -r z; do
    z=${z%%#*}; z=${z%"${z##*[![:space:]]}"}
    [ -n "$z" ] && DIRS+=("${z/#\~/$HOME}")
  done < "$LOCAL/check-dirs.lokal"
fi
[ ${#DIRS[@]} -gt 0 ] || { echo "No test folders: set GRAINEO_TEST_DATA or create $LOCAL/check-dirs.lokal"; exit 2; }

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/data"
export GRAINEO_DATA_DIR="$WORK/data" GRAINEO_NO_DISK_CACHE=1 GRAINEO_NO_PHOTOKIT=1 GRAINEO_LIBRARY_NO_SYNC=1 GRAINEO_LIBRARY_SIDECARS=0

find_file() {
  local d
  for d in "${DIRS[@]}"; do
    [ -f "$d/$1" ] && { echo "$d/$1"; return 0; }
  done
  return 1
}

compute() { # compute <kind> <path>
  case "$1" in
    spphash) "$DEV" spphash "$2" 2>/dev/null | cut -c1-16 ;;
    color | mono)
      local extra=()
      [ "$1" = mono ] && extra=(--mono)
      rm -f "$WORK/image.tif"
      "$DEV" develop "$2" "$WORK/image.tif" --spp --baseline ${extra[@]+"${extra[@]}"} > "$WORK/log.txt" 2>&1 \
        && python3 -I "$ROOT/scripts/tiff_pixel_hash.py" "$WORK/image.tif" ;;
    *) echo "unknown kind" ;;
  esac
}

# Disk cache guard: the stored Sigma bases are keyed on COMPUTE_PATH_VERSION (crates/develop/src/diskcache.rs), not on
# the program. Changed reference checksums mean a changed base, so the version must have been counted up: against the
# header line of sigma.txt (digest of its checksums) and against the last release tag (checksums changed since then →
# a higher version than then).
cache_guard() {
  python3 -I - "$ROOT" <<'PY'
import re, subprocess, sys
root = sys.argv[1]
REF = "crates/graineo-dev/reference/sigma.txt"
CODE = "crates/develop/src/diskcache.rs"

def digest(text):
    h = 0xcbf29ce484222325
    lines = "".join(" ".join(l.split()[:3]) + "\n" for l in map(str.strip, text.splitlines()) if l and not l.startswith("#"))
    for b in lines.encode():
        h = ((h ^ b) * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
    return h

def header(text):
    m = re.search(r"^# compute-path-version: (\d+) digest: ([0-9a-f]{16})", text, re.M)
    return (int(m.group(1)), int(m.group(2), 16)) if m else None

def code_version(text):
    m = re.search(r"pub const COMPUTE_PATH_VERSION: u32 = (\d+);", text or "")
    return int(m.group(1)) if m else 0

ref = open(f"{root}/{REF}").read()
code = code_version(open(f"{root}/{CODE}").read())
h = header(ref)
problems = []
if h is None:
    problems.append(f"{REF}: header line '# compute-path-version: <n> digest: <hex>' missing")
else:
    if h[1] != digest(ref):
        problems.append(f"{REF}: checksums changed – increase COMPUTE_PATH_VERSION (now {code}) and write '# compute-path-version: {code + 1} digest: {digest(ref):016x}'")
    if h[0] != code:
        problems.append(f"{REF} names version {h[0]}, {CODE} has {code}")
git = lambda *a: subprocess.run(["git", "-C", root, *a], capture_output=True, text=True)
tag = git("describe", "--tags", "--abbrev=0", "--match", "v*").stdout.strip()
if tag:
    # the reference file had a German path before 1.0.1
    old_ref = next((t for t in (git("show", f"{tag}:{p}").stdout for p in (REF, "crates/graineo-dev/referenz/sigma.txt")) if t), "")
    old_code = code_version(git("show", f"{tag}:{CODE}").stdout)
    changed = not old_ref or digest(old_ref) != digest(ref)
    if changed and code <= old_code:
        problems.append(f"checksums changed since {tag}, but COMPUTE_PATH_VERSION is {code} (then {old_code}): count it up")
    print(f"cache guard: COMPUTE_PATH_VERSION {code}, last release {tag} had {old_code}, checksums {'changed' if changed else 'unchanged'} since then")
else:
    print(f"cache guard: COMPUTE_PATH_VERSION {code} (no release tag found)")
for p in problems:
    print("CACHE GUARD: " + p)
sys.exit(1 if problems else 0)
PY
}
guard_ok=1
cache_guard || guard_ok=0

same=0 differ=0 missing=0
for ref in "${REFS[@]}"; do
  while read -r kind file expected _; do
    case "$kind" in '' | '#'*) continue ;; esac
    if ! path=$(find_file "$file"); then
      missing=$((missing + 1))
      echo "missing   $kind $file"
      continue
    fi
    actual=$(compute "$kind" "$path" < /dev/null)
    if [ "$actual" = "$expected" ]; then
      same=$((same + 1))
      echo "same      $kind $file"
    else
      differ=$((differ + 1))
      echo "DIFFERENT $kind $file: $actual (reference $expected)"
    fi
  done < "$ref"
done

echo "same: $same, different: $differ, not found: $missing"
if [ "$differ" -gt 0 ] || [ "$guard_ok" -eq 0 ]; then
  echo "CHECK FAILED"
  exit 1
fi
if [ "$same" -eq 0 ]; then
  echo "Nothing checked (no test file found)"
  exit 2
fi
echo "CHECK PASSED"
