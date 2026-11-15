#!/bin/bash
# Measurement of the compute scheduling:
# wait times at the gates (log lines "gate <name>: waited <ms> ms (lane <thread>)"), hover storm, browsing.
# Usage: scripts/scheduling-measure.sh <output_folder> <photo_folder_with_copies> [a|b|c|all]
#   (a) browsing/zooming with sliders (GRAINEO_STRESS=7, N=150, STRESS_EDIT), warm disk cache
#   (b) like (a) with an empty disk cache and an empty library (bases are computed)
#   (c) hover storm: 30 films in 3 s (GRAINEO_STRESS_HOVER=1)
#   BIN=…  program (default: $CARGO_TARGET_DIR/release/graineo)   SEED=7  N=150
# Own test folders, never the real library. Evaluate with: scripts/perf-evaluate.py <output_folder>
OUT=${1:?output folder}; PHOTOS=${2:?folder with copies}; WHICH=${3:-all}
START=$(ls "$PHOTOS"/*.[Xx]3[Ff] 2>/dev/null | head -1)
[ -n "$START" ] || { echo "no start photo"; exit 2; }
BIN=${BIN:-${CARGO_TARGET_DIR:-"$(dirname "$0")/../target"}/release/graineo}
SEED=${SEED:-7}; N=${N:-150}
mkdir -p "$OUT"
DATA=$OUT/data; CACHE=$OUT/cache
echo "Load before the run: $(uptime)" | tee -a "$OUT/load.txt"
run() { # name, extra env …
  local name=$1; shift
  local D=$OUT/$name; mkdir -p "$D"
  env GRAINEO_DATA_DIR=$DATA GRAINEO_CACHE_DIR=$CACHE GRAINEO_LIBRARY_SIDECARS=0 GRAINEO_LIBRARY_NO_SYNC=1 GRAINEO_NO_PHOTOKIT=1 \
    GRAINEO_NO_SPLASH=1 GRAINEO_LOG=1 GRAINEO_FRAME_LOG=1 GRAINEO_STRESS=$SEED "$@" \
    /usr/bin/time -l "$BIN" "$START" > "$D/stdout.log" 2> "$D/stderr.log"
  local code=$?
  echo "$code" > "$D/exitcode.txt"
  cp "$DATA/logs/trace.log" "$D/trace.log" 2>/dev/null
  grep -E "maximum resident set size|real|peak memory footprint" "$D/stderr.log" > "$D/time.log"
  echo "$name: exit code $code, $(grep -c '^gate \|\] gate ' "$D/stderr.log") wait lines, $(grep -h 'stress: ' "$D/stderr.log" | tail -1)"
}
case $WHICH in
  b|all) rm -rf "$DATA" "$CACHE"; mkdir -p "$DATA" "$CACHE"; run b GRAINEO_TAB=develop GRAINEO_STRESS_N=$N GRAINEO_STRESS_EDIT=1 ;;
esac
case $WHICH in
  a|all) mkdir -p "$DATA" "$CACHE"; run a GRAINEO_TAB=develop GRAINEO_STRESS_N=$N GRAINEO_STRESS_EDIT=1 ;;
esac
case $WHICH in
  c|all) mkdir -p "$DATA" "$CACHE"; run c GRAINEO_TAB=darkroom GRAINEO_STRESS_HOVER=1 ;;
esac
echo "Load after the run: $(uptime)" | tee -a "$OUT/load.txt"
