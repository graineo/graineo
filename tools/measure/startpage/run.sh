#!/bin/bash
# Start page benchmarks in one unattended run: library grid with many photos (scrolling, jumps), import (window
# thumbnails, photos per second, frame times), single photo (first image, full quality, slider, zoom/pan without
# recomputing). Writes one result: <work>/result/result.txt and result.json (with computer and load facts).
#
# Usage:
#   GRAINEO_APP=/path/release/graineo GRAINEO_DEV=/path/release/graineo-dev \
#     ./run.sh <work folder> <folder with test RAWs>
#
# The test RAWs are only read (copied into the work folder). Test data (made once, kept in the work folder for later
# runs; delete the work folder to make them again):
#   library/     synthetic library with PHOTOS photos and their previews (graineo-dev bench-data library)
#   import-src/  JPEGS generated JPEGs and RAWS RAW copies (graineo-dev bench-data import-source)
#
# Settings (environment):
#   PHOTOS=65000 JPEGS=2000 RAWS=200  test data size
#   GRID_RUNS=3                        grid runs (for the spread)
#   GRID="speeds=5,10,20,40,80,160;secs=6;jumps=24"   grid program (see crates/app/src/grid_bench.rs)
#   TIMING_N=2                         warm runs per photo for open/full/slider (plus one cold)
#   PARTS="grid import large"          which parts to run
#
# Never the real library: every app run has its own GRAINEO_DATA_DIR inside the work folder, no XMP, no sync, no
# Apple Photos. Keep the computer quiet during the run (no builds, no other measurements), the screen on and the app
# window visible: covered windows do not draw. Takes 10–20 minutes (plus the test data once).
# License: GPL-3.0-only.
set -u
WORK=${1:?work folder missing}
SOURCES=${2:?folder with test RAWs missing}
APP=${GRAINEO_APP:?GRAINEO_APP (program graineo) missing}
DEV=${GRAINEO_DEV:?GRAINEO_DEV (program graineo-dev) missing}
HERE=$(cd "$(dirname "$0")" && pwd)
PHOTOS=${PHOTOS:-65000}
JPEGS=${JPEGS:-2000}
RAWS=${RAWS:-200}
GRID_RUNS=${GRID_RUNS:-3}
GRID=${GRID:-speeds=5,10,20,40,80,160;secs=6;jumps=24}
TIMING_N=${TIMING_N:-2}
PARTS=${PARTS:-grid import large}
mkdir -p "$WORK"
WORK=$(cd "$WORK" && pwd)
OUT="$WORK/result"
rm -rf "$OUT"   # only the result of an earlier run of this script
mkdir -p "$OUT"
LOG="$OUT/run.log"
say() { echo "$(date +%H:%M:%S) $*" | tee -a "$LOG"; }
has() { case " $PARTS " in *" $1 "*) return 0 ;; esac; return 1; }

# computer facts and load (2 s of processor use, load average, top processes) into <file>.json and .top
facts() {
    "$DEV" bench-data system --busy-secs 2 --path "$WORK" > "$1.json" 2>> "$LOG"
    LC_ALL=C ps -Ao pcpu,comm -r | sed -n 2,6p | awk '{c=$0; sub(/^ *[0-9.]+ +/, "", c); n=split(c, a, "/"); printf "%s %s%%\n", a[n], $1}' > "$1.top"
}

app_env() { # data folder, then further VAR=value …
    local d=$1; shift
    env GRAINEO_DATA_DIR="$d" GRAINEO_NO_XMP=1 GRAINEO_LIBRARY_NO_SYNC=1 GRAINEO_LIBRARY_SIDECARS=0 \
        GRAINEO_NO_PHOTOKIT=1 GRAINEO_NO_SPLASH=1 GRAINEO_NO_CAMERA=1 "$@"
}

say "start: $(uptime)"
facts "$OUT/system-before"

# ── test data (once)
if has grid && [ ! -f "$WORK/library/library.sqlite" ]; then
    say "making the synthetic library ($PHOTOS photos) …"
    "$DEV" bench-data library "$WORK/library" --sources "$SOURCES" --photos "$PHOTOS" >> "$LOG" 2>&1 || { say "library failed"; exit 1; }
fi
if has import && [ ! -d "$WORK/import-src/raw" ]; then
    say "making the import source ($JPEGS JPEGs, $RAWS RAW copies) …"
    "$DEV" bench-data import-source "$WORK/import-src" --sources "$SOURCES" --jpegs "$JPEGS" --raws "$RAWS" >> "$LOG" 2>&1 || { say "import source failed"; exit 1; }
fi

# ── 1. library grid
if has grid; then
    # warm-up start (not measured): first-start work of the app (daily backup of the library, tidying the
    # previews) and the file cache of the system, the same for every measured run
    say "grid: warm-up start (40 s, not measured)"
    app_env "$WORK/library" GRAINEO_QUIT=40 GRAINEO_TAB=library "$APP" > "$OUT/grid-warmup.log" 2>&1 </dev/null
    for r in $(seq 1 "$GRID_RUNS"); do
        say "grid: run $r of $GRID_RUNS"
        facts "$OUT/grid-$r-system"
        app_env "$WORK/library" GRAINEO_QUIT=600 GRAINEO_TAB=library GRAINEO_LOG=1 \
            GRAINEO_GRID_BENCH="out=$OUT/grid-$r.json;$GRID" "$APP" > "$OUT/grid-$r.log" 2>&1 </dev/null
        [ -f "$OUT/grid-$r.json" ] || say "grid: run $r gave no result (see grid-$r.log)"
    done
fi

# ── 2. import: JPEGs copied, RAWs added in place
if has import; then
    for kind in jpeg raw; do
        mode=copy; [ "$kind" = raw ] && mode=inplace
        d="$WORK/import-data-$kind"; target="$WORK/import-target-$kind"
        rm -rf "$d" "$target"   # own scratch folders of this script
        mkdir -p "$d" "$target"
        printf '{\n  "importDest": "%s",\n  "importRawOnly": false\n}\n' "$target" > "$d/settings.json"
        say "import: $kind ($mode)"
        facts "$OUT/import-$kind-system"
        # the window opens on the folder at the start; the import starts after 8 s (the window's grid has
        # filled by then), afterwards the grid shows the imported photos
        app_env "$d" GRAINEO_QUIT=900 GRAINEO_TAB=library GRAINEO_LOG=1 \
            GRAINEO_LIBRARY_TEST="import=$WORK/import-src/$kind" GRAINEO_IMPORT_MODE=$mode \
            GRAINEO_IMPORT_YES=8 GRAINEO_IMPORT_SHOW=0 \
            GRAINEO_IMPORT_BENCH="out=$OUT/import-$kind.json;timeout=850" "$APP" > "$OUT/import-$kind.log" 2>&1 </dev/null
        du -sk "$WORK/import-src/$kind" | awk '{print $1 * 1024}' > "$OUT/import-$kind.bytes"
        [ -f "$OUT/import-$kind.json" ] || say "import: $kind gave no result (see import-$kind.log)"
        rm -rf "$target"   # the copies (several GB)
    done
fi

# ── 3. single photo per camera family
if has large; then
    cases="$OUT/cases.tsv"
    : > "$cases"
    for f in "$SOURCES"/*; do
        n=$(basename "$f"); u=$(echo "$n" | tr '[:lower:]' '[:upper:]')
        case "$u" in
            SDQ*.X3F) c=quattro ;;
            *.X3F) c=merrill ;;
            *.RAF) c=xtrans ;;
            *.CR3|*.CR2) c=canon ;;
            R0*.DNG|R3*.DNG) c=ricoh-dng ;;
            L1*.DNG) c=leica-dng ;;
            *.DNG) c=dng ;;
            *.ARW) c=sony ;;
            *.NEF) c=nikon ;;
            *) continue ;;
        esac
        grep -q "^$c	" "$cases" || printf '%s\t%s\n' "$c" "$f" >> "$cases"
    done
    say "large view: $(cut -f1 "$cases" | tr '\n' ' ')"
    facts "$OUT/large-system"
    GRAINEO_TIMING_NO_EXPORT=1 "$HERE/../timing/timing.sh" "$cases" "$OUT/timing" "$TIMING_N" >> "$LOG" 2>&1
    # zoom guard: open, zoom to 800 %, wait until the whole image is in place, then 200 zoom/pan steps; counts the
    # render jobs sent afterwards (expected 0) and the frame times meanwhile
    while IFS=$'\t' read -r c f; do
        d="$WORK/zoom-data"; rm -rf "$d"; mkdir -p "$d/photo"
        cp "$f" "$d/photo/"
        say "zoom/pan: $c"
        app_env "$d" GRAINEO_CACHE_DIR="$d/cache" GRAINEO_TAB=develop GRAINEO_STRESS=7 GRAINEO_STRESS_ZOOM=1 \
            GRAINEO_FRAME_LOG=1 GRAINEO_QUIT=240 "$APP" "$d/photo/$(basename "$f")" > "$OUT/zoom-$c.log" 2>&1 </dev/null
        echo $? > "$OUT/zoom-$c.exit"
        rm -rf "$d"
    done < "$cases"
fi

facts "$OUT/system-after"
say "end: $(uptime)"
python3 -I "$HERE/evaluate.py" "$OUT"
