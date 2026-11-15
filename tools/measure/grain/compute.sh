#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-only
# Renders step wedge, gradient and photo per film with the test program (graineo-dev grain) in the modes
# today / upstream (partial-layer grain like spektrafilm Python, colour films only) / new (grain size classes), as a print.
# Usage: GRAINEO_DEV=<graineo-dev> GRAINEO_FILM_DATA=<spektrafilm-rs/data> WORKDIR=<folder with wedge.png, gradient.png, photo.png> compute.sh
# Then: apply grain_measure.py / wedge_measure.py to the generated PNGs.
set -e
: "${GRAINEO_DEV:?set GRAINEO_DEV (path to graineo-dev)}"
: "${GRAINEO_FILM_DATA:?set GRAINEO_FILM_DATA (folder third_party/spektrafilm-rs/data)}"
: "${WORKDIR:?set WORKDIR (folder with wedge.png, gradient.png, photo.png; output goes there)}"
export GRAINEO_DATA_DIR="${GRAINEO_DATA_DIR:-$(mktemp -d)}"   # never the real library
export GRAINEO_FILM_DATA
B="$GRAINEO_DEV"
cd "$WORKDIR"
for f in fujifilm_acros_100_ii kodak_trix_400 ilford_hp5_plus kodak_portra_400 fujifilm_velvia_100; do
  modes="today new"
  case $f in kodak_*portra*|fujifilm_velvia*) modes="today upstream new";; esac
  for m in $modes; do
    unset GRAINEO_GRAIN_CLASSES GRAINEO_GRAIN_LAYERS
    [ $m = new ] && export GRAINEO_GRAIN_CLASSES=1
    [ $m = upstream ] && export GRAINEO_GRAIN_LAYERS=1
    for b in wedge gradient photo; do
      $B grain $f --image $b.png --out ${f}_${m}_${b}.png --format 14.4 --print
    done
    # negative scanned directly (film density, wedge only)
    $B grain $f --image wedge.png --out ${f}_${m}_wedgeneg.png --format 14.4
  done
done
# GPU/CPU cross-check
unset GRAINEO_GRAIN_CLASSES GRAINEO_GRAIN_LAYERS
GRAINEO_GRAIN_LAYERS=1 GRAINEO_FILM_CPU=1 $B grain kodak_portra_400 --image wedge.png --out kodak_portra_400_upstream_wedge_cpu.png --format 14.4 --print
GRAINEO_GRAIN_CLASSES=1 GRAINEO_FILM_CPU=1 $B grain kodak_portra_400 --image wedge.png --out kodak_portra_400_new_wedge_cpu.png --format 14.4 --print
