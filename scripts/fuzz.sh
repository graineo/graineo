#!/bin/bash
# Longer fuzz runs of the readers of foreign files (stable Rust, no extra toolchain).
#
# The same targets run briefly (400 inputs each) in every `cargo test`. This script runs them for a
# given time per target, one target at a time with at most two threads, so the machine stays quiet.
#
#   scripts/fuzz.sh                 # every target for 120 s
#   scripts/fuzz.sh 600             # every target for 10 min (the upper bound we use)
#   scripts/fuzz.sh 300 x3f_camf    # only targets whose test name contains "x3f_camf"
#
# Environment: CARGO_TARGET_DIR (own build folder), GRAINEO_BUILD_LOCK (command prefix for cargo), GRAINEO_FUZZ_SEED (other random start),
# GRAINEO_FUZZ_OUT (folder for crashing inputs, default: target folder/fuzz-crashes).
# A crashing input is saved and can be replayed with GRAINEO_FUZZ_REPLAY=<file> and the test name.
#
# Targets (test name → reader):
#   fuzz_x3f_container            x3f: whole file (directory, sections, CAMF, PROP, preview, RAW headers)
#   fuzz_x3f_camf / _prop         x3f: CAMF (types 2/4/5) and PROP sections directly
#   fuzz_x3f_jpeg_exif            x3f: EXIF of the embedded JPEG
#   fuzz_onnx_small / _yunet      faces: ONNX reader (+ graph and net construction for the small model)
#   fuzz_focus_and_picture_style  raw: focus points and picture style from makernotes (CR2/CR3/ARW/DNG/RAF/HEIC/JPEG)
#   fuzz_lightroom_catalog_text   rawdev: catalog develop text and .lrtemplate presets → settings, spots, masks
#   fuzz_lightroom_xmp            rawdev: crs: fields of XMP sidecars and XMP presets → settings, spots, masks
#   fuzz_xmp_sidecar              library: sidecars (ours and foreign), merging our fields in
#   fuzz_lightroom_develop_text   library: as-shot line of handed-over develop texts
#   fuzz_exif_header              library: EXIF of JPEG/PNG/TIFF, embedded XMP of a JPEG
set -euo pipefail
cd "$(dirname "$0")/.."

SECS="${1:-120}"
FILTER="${2:-}"
if [ "$SECS" -gt 600 ]; then
  echo "fuzz.sh: limiting to 600 s per target" >&2
  SECS=600
fi
export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"
# optional wrapper that limits parallel builds (e.g. a shared build lock script)
LOCK="${GRAINEO_BUILD_LOCK:-}"
TARGET_DIR="${CARGO_TARGET_DIR:-target}"
export GRAINEO_FUZZ_OUT="${GRAINEO_FUZZ_OUT:-$TARGET_DIR/fuzz-crashes}"
export GRAINEO_FUZZ_SECS="$SECS" GRAINEO_FUZZ_VERBOSE=1 RAYON_NUM_THREADS=2
# quick build profile (the fuzz runs do not measure anything)
export CARGO_PROFILE_RELEASE_STRIP=none CARGO_PROFILE_RELEASE_DEBUG=0 CARGO_PROFILE_RELEASE_LTO=off CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16

run() { # package, test selector args..., name filter
  local pkg="$1"; shift
  local names="$1"; shift
  for n in $names; do
    if [ -n "$FILTER" ] && [[ "$n" != *"$FILTER"* ]]; then continue; fi
    echo "== $pkg $n (${SECS} s)"
    nice -n 10 $LOCK cargo test --release -j 2 -p "$pkg" "$@" "$n" -- --exact --nocapture --test-threads 1 2>&1 \
      | grep -E "inputs in|test result|panicked|saved|replay|abort" || { echo "!! $pkg $n failed"; exit 1; }
  done
}

run x3f "fuzz_x3f_container fuzz_x3f_camf fuzz_x3f_prop fuzz_x3f_jpeg_exif" --test fuzz
run faces "fuzz_onnx_small fuzz_onnx_yunet" --test fuzz
run raw "focus::tests::fuzz_focus_and_picture_style" --lib
run rawdev "fuzz_lightroom_catalog_text fuzz_lightroom_xmp" --test fuzz
run library "fuzz_tests::fuzz_xmp_sidecar fuzz_tests::fuzz_lightroom_develop_text fuzz_tests::fuzz_exif_header" --lib
echo "fuzz.sh: all targets ran ${SECS} s without findings"
