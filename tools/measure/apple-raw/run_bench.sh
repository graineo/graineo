#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-only
#
# run_bench.sh – performance of the system RAW engine (apple-raw-bench) and, interleaved under the same load,
# graineo's own pipeline (graineo-dev rawapp --full), one fresh process per measurement.
#
# Usage: GRAINEO_DEV=/path/to/graineo-dev ./run_bench.sh BENCH_BINARY OUTDIR RUNS FILE…
#   BENCH_BINARY  compiled apple_raw_bench.swift
#   OUTDIR        result folder (bench.tsv, export JPEGs, load.txt)
#   RUNS          processes per scenario (3 recommended)
# graineo-dev runs with its own GRAINEO_DATA_DIR inside OUTDIR and never writes XMP sidecars.
set -u
BENCH=$1; OUT=$2; RUNS=$3; shift 3
mkdir -p "$OUT/data" "$OUT/export"
export GRAINEO_DATA_DIR="$OUT/data" GRAINEO_NO_XMP=1 GRAINEO_NO_PHOTOKIT=1 GRAINEO_LIBRARY_NO_SYNC=1
R="$OUT/bench.tsv"
: > "$R"
uptime > "$OUT/load.txt"
for f in "$@"; do
  b=$(basename "$f")
  for r in $(seq 1 "$RUNS"); do
    echo "# $b run $r $(uptime | sed 's/.*averages: //')" >> "$R"
    "$BENCH" first "$f" --px 2048 --runs 1 >> "$R"
    "$BENCH" full "$f" --runs 3 >> "$R"
    "$BENCH" full "$f" --cpu --runs 2 >> "$R"
    "$BENCH" slider "$f" --px 1600 --steps 20 >> "$R"
    "$BENCH" export "$f" "$OUT/export/${b%.*}.jpg" --runs 2 >> "$R"
    if [ -n "${GRAINEO_DEV:-}" ]; then
      # graineo: preview stage + slider runs + full resolution, as in the app (crates/rawdev); /usr/bin/time for memory
      { /usr/bin/time -l "$GRAINEO_DEV" rawapp "$f" "$OUT/export/${b%.*}-graineo.jpg" --full; } 2>&1 \
        | grep -E "open|maximum resident|peak memory footprint" | sed "s/^/GRAINEO\t$b\t/" >> "$R"
    fi
  done
done
uptime >> "$OUT/load.txt"
