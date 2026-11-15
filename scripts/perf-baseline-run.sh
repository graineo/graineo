#!/bin/bash
# Performance baseline run: stress test with slider dragging, once cold (empty disk cache), once with a filled cache.
# Own test folders, never the real library. Only use copies of photos.
# Usage: scripts/perf-baseline-run.sh <output_folder> <photo_folder_with_copies> [start_photo]
#   BIN=…        another program (default: target/release/graineo, or $CARGO_TARGET_DIR)
#   SEED=7 N=150 random sequence and number of steps
# Result per run: <output_folder>/<cold|warm>/{trace.log,stderr.log,time.log}; evaluate with scripts/perf-evaluate.py
OUT=${1:?output folder}; PHOTOS=${2:?folder with copies}
START=${3:-$(ls "$PHOTOS"/*.[Xx]3[Ff] 2>/dev/null | head -1)}
[ -n "$START" ] || { echo "no start photo"; exit 2; }
BIN=${BIN:-${CARGO_TARGET_DIR:-"$(dirname "$0")/../target"}/release/graineo}
SEED=${SEED:-7}; N=${N:-150}
mkdir -p "$OUT"
DATA=$OUT/data; CACHE=$OUT/cache
rm -rf "$DATA" "$CACHE"   # only this script's own scratch folder
mkdir -p "$DATA" "$CACHE"
echo "Load before the run: $(uptime)" | tee "$OUT/load.txt"
for run in cold warm; do
  D=$OUT/$run; mkdir -p "$D"
  # cold: empty disk cache and fresh library; warm: keep cache and library from the first run
  [ "$run" = cold ] && { rm -rf "$DATA"/* "$CACHE"/*; }
  GRAINEO_DATA_DIR=$DATA GRAINEO_CACHE_DIR=$CACHE GRAINEO_LIBRARY_SIDECARS=0 GRAINEO_LIBRARY_NO_SYNC=1 GRAINEO_NO_PHOTOKIT=1 \
    GRAINEO_NO_SPLASH=1 GRAINEO_TAB=develop GRAINEO_STRESS=$SEED GRAINEO_STRESS_N=$N GRAINEO_STRESS_EDIT=1 GRAINEO_FRAME_LOG=1 \
    /usr/bin/time -l "$BIN" "$START" > "$D/stdout.log" 2> "$D/stderr.log"
  code=$?
  echo "$code" > "$D/exitcode.txt"
  cp "$DATA/logs/trace.log" "$D/trace.log" 2>/dev/null
  grep -E "maximum resident set size|real|peak memory footprint" "$D/stderr.log" > "$D/time.log"
  echo "$run: exit code $code, $(tail -1 "$D/time.log" | tr -s ' ')"
done
echo "Load after the run: $(uptime)" | tee -a "$OUT/load.txt"
