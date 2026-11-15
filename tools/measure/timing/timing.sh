#!/bin/bash
# Timing for the project website: open to first image, full quality, recompute after a slider,
# export at full resolution (JPEG). Per case one cold run (separate) and N warm runs (median, min, max).
#
# Usage:
#   GRAINEO_APP=/path/graineo GRAINEO_DEV=/path/graineo-dev \
#     ./timing.sh <cases.tsv> <output-folder> [N=5]
#
# cases.tsv: one line per case, `name<TAB>path to file` (# = comment). Each file is copied into its own folder under
# <output-folder>/photos (the app only reads the copy; one folder per case so that the app does not
# preload neighbours). Every run gets a fresh GRAINEO_DATA_DIR (no disk cache, no previews):
# so what is measured is always the first opening of a new photo.
#
# Export as in the app: Sigma with "best quality" (Sigma base, camera defaults: --baseline --spp),
# other RAWs through the same export path (rawdev::export) with default values.
# GRAINEO_TIMING_EXPORT_ONLY=1 measures only the exports.
#
# The real library is never touched (own data folder, no XMP, no sync, no Apple Photos).
# License: GPL-3.0-only.
set -u
CASES=${1:?cases.tsv missing}
OUT=${2:?output folder missing}
N=${3:-5}
APP=${GRAINEO_APP:?GRAINEO_APP (program graineo) missing}
DEV=${GRAINEO_DEV:?GRAINEO_DEV (program graineo-dev) missing}
HERE=$(cd "$(dirname "$0")" && pwd)
# Wait time until the app quits per run (s); large files need longer for the full resolution
QUIT=${GRAINEO_TIMING_QUIT:-14}
QUIT_LARGE=${GRAINEO_TIMING_QUIT_LARGE:-24}

mkdir -p "$OUT/photos" "$OUT/logs" "$OUT/export"
LAST="$OUT/load.tsv"
: > "$LAST"

note_load() { # note system load and CPU share of a running graineo app (not ours)
    local block=$1
    local l; l=$(uptime | sed 's/.*load averages*: //')
    local other; other=$(LC_ALL=C ps -Ao pid,pcpu,comm | awk -v app="$APP" '$3 ~ /graineo$/ && $3 != app {s+=$2} END {printf "%.0f", s+0}')
    local top; top=$(LC_ALL=C ps -Ao pcpu,comm -r | sed -n 2,4p | awk '{c=$0; sub(/^ *[0-9.]+ +/, "", c); n=split(c, a, "/"); printf "%s %s%%; ", a[n], $1}')
    printf '%s\t%s\t%s\t%s\t%s\n' "$(date +%H:%M:%S)" "$block" "$l" "$other" "$top" >> "$LAST"
}

env_app() {
    env GRAINEO_DATA_DIR="$1" GRAINEO_NO_XMP=1 GRAINEO_LIBRARY_NO_SYNC=1 GRAINEO_LIBRARY_SIDECARS=0 \
        GRAINEO_NO_PHOTOKIT=1 GRAINEO_NO_SPLASH=1 GRAINEO_LOG=1 GRAINEO_EDIT_LOG=1 GRAINEO_TAB=develop \
        GRAINEO_IMAGE="$2" GRAINEO_SET_EV=0.5 GRAINEO_QUIT="$3" "$APP"
}

grep -v '^#' "$CASES" | while IFS=$'\t' read -r name path; do
    [ -z "$name" ] && continue
    mkdir -p "$OUT/photos/$name"
    file="$OUT/photos/$name/$(basename "$path")"
    [ -f "$file" ] || cp "$path" "$file"
    q=$QUIT
    case "$name" in large*) q=$QUIT_LARGE ;; esac
    note_load "app $name"
    [ -n "${GRAINEO_TIMING_EXPORT_ONLY:-}" ] || for i in $(seq 0 "$N"); do   # 0 = cold
        d=$(mktemp -d "$OUT/data.XXXX")
        env_app "$d" "$file" "$q" 2>&1 </dev/null | python3 -I "$HERE/stamp.py" > "$OUT/logs/$name.app.$i.log"
        rm -rf "$d"
    done
    note_load "export $name"
    for i in $(seq 0 "$N"); do
        d=$(mktemp -d "$OUT/data.XXXX")
        target="$OUT/export/$name.jpg"
        rm -f "$target"
        t0=$(python3 -c 'import time; print(time.monotonic())')
        case "$file" in
            *.X3F|*.x3f) GRAINEO_DATA_DIR="$d" GRAINEO_CACHE_DIR="$d/cache" "$DEV" develop "$file" "$target" --baseline --spp > "$OUT/logs/$name.export.$i.log" 2>&1 ;;
            *) GRAINEO_DATA_DIR="$d" GRAINEO_CACHE_DIR="$d/cache" "$DEV" color-export "$file" "$target" > "$OUT/logs/$name.export.$i.log" 2>&1 ;;
        esac
        t1=$(python3 -c 'import time; print(time.monotonic())')
        size=$(sips -g pixelWidth -g pixelHeight "$target" 2>/dev/null | awk '/pixel/ {printf "%s ", $2}')
        python3 -c "print(f'EXPORT\t{($t1-$t0)*1000:.0f}\t$size')" >> "$OUT/logs/$name.export.$i.log"
        rm -rf "$d"
    done
done
note_load "end"
python3 -I "$HERE/evaluate.py" "$OUT" "$CASES"
