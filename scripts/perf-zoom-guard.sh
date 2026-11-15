#!/bin/bash
# Zoom guard: open a photo, zoom, wait until the whole image is ready, then 200 zoom/pan
# actions. Expected: log line "stress: zoom jobs region=0 full=0 frame_p95=<ms>".
# Usage: scripts/perf-zoom-guard.sh <output_folder> <photo (copy!)> [mask|darkroom]
#   BIN=…  program (default: target/release/graineo)   SEED=7
OUT=${1:?output folder}; PHOTO=${2:?photo}; MASK=${3:-}
BIN=${BIN:-${CARGO_TARGET_DIR:-"$(dirname "$0")/../target"}/release/graineo}
SEED=${SEED:-7}
D=$OUT/zoom; DATA=$OUT/data; CACHE=$OUT/cache
rm -rf "$DATA" "$CACHE" "$D"   # only this script's own scratch folder
mkdir -p "$DATA" "$CACHE" "$D"
echo "Load before the run: $(uptime)" | tee "$OUT/load.txt"
ZOOM=1; [ "$MASK" = mask ] && ZOOM=mask
TAB=develop; if [ "$MASK" = darkroom ]; then TAB=darkroom; export GRAINEO_DARKROOM_FILM=kodak_trix_400; fi
GRAINEO_DATA_DIR=$DATA GRAINEO_CACHE_DIR=$CACHE GRAINEO_LIBRARY_SIDECARS=0 GRAINEO_LIBRARY_NO_SYNC=1 GRAINEO_NO_PHOTOKIT=1 \
  GRAINEO_NO_SPLASH=1 GRAINEO_TAB=$TAB GRAINEO_STRESS=$SEED GRAINEO_STRESS_ZOOM=$ZOOM GRAINEO_FRAME_LOG=1 \
  /usr/bin/time -l "$BIN" "$PHOTO" > "$D/stdout.log" 2> "$D/stderr.log"
code=$?
echo "$code" > "$D/exitcode.txt"
cp "$DATA/logs/trace.log" "$D/trace.log" 2>/dev/null
grep -E "maximum resident set size|real|peak memory footprint" "$D/stderr.log" > "$D/time.log"
echo "Load after the run: $(uptime)" | tee -a "$OUT/load.txt"
echo "Exit code $code; $(grep -h 'stress: zoom jobs' "$D/stderr.log" | tail -1)"
