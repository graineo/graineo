#!/bin/bash
# Format check: runs `graineo-dev formats` over a folder of sample files (grid, previews, metadata, develop,
# export, import into a scratch library) and writes the raw lines (TSV) and a Markdown table.
#
# usage: scripts/format-check.sh <folder with sample files> [output folder] [--no-export] [--no-import]
#
# The sample files are not part of the repository (free sources, e.g. the CC0 files of raw.pixls.us; see the
# internal notes on formats). Everything the check writes goes to the output folder (default: a new temp folder):
# its own GRAINEO_DATA_DIR, the developed and embedded previews (`out/`), the import library (`import/`),
# `formats.tsv` and `formats.md`. Nothing is written next to the sample files (GRAINEO_NO_XMP=1).
# Program: $GRAINEO_DEV, else graineo-dev in $CARGO_TARGET_DIR/release, else target/release.
set -euo pipefail

src=${1:?"usage: $0 <folder with sample files> [output folder] [--no-export] [--no-import]"}
shift
out=""
export_flag="--export"
import=1
for a in "$@"; do
  case "$a" in
    --no-export) export_flag="" ;;
    --no-import) import=0 ;;
    *) out=$a ;;
  esac
done
[ -d "$src" ] || { echo "not a folder: $src" >&2; exit 1; }
[ -n "$out" ] || out=$(mktemp -d "${TMPDIR:-/tmp}/graineo-format-check.XXXXXX")
mkdir -p "$out/data" "$out/out"

root=$(cd "$(dirname "$0")/.." && pwd)
dev=${GRAINEO_DEV:-${CARGO_TARGET_DIR:-$root/target}/release/graineo-dev}
[ -x "$dev" ] || { echo "graineo-dev not found ($dev): build it first or set GRAINEO_DEV" >&2; exit 1; }

args=("$src" --out "$out/out")
[ -n "$export_flag" ] && args+=("$export_flag")
if [ "$import" = 1 ]; then
  rm -rf "$out/import"
  mkdir -p "$out/import"
  args+=(--import "$out/import")
fi

GRAINEO_DATA_DIR="$out/data" GRAINEO_NO_XMP=1 GRAINEO_NO_PHOTOKIT=1 "$dev" formats "${args[@]}" >"$out/formats.tsv" 2>"$out/formats.log"

# Markdown table: one row per file, the columns of the TSV (without the leading FORMAT tag)
awk -F'\t' '
  $1 != "FORMAT" { next }
  {
    row = "|"
    for (i = 2; i <= NF; i++) { v = $i; gsub(/\|/, "/", v); row = row " " v " |" }
    print row
    if (NR == 1) { sep = "|"; for (i = 2; i <= NF; i++) sep = sep " --- |"; print sep }
  }' "$out/formats.tsv" >"$out/formats.md"

errors=$(awk -F'\t' '$1 == "FORMAT" && NR > 1 && ($4 == "-" || $0 ~ /ERR|PANIC/)' "$out/formats.tsv" | wc -l | tr -d ' ')
files=$(awk -F'\t' '$1 == "FORMAT" && NR > 1' "$out/formats.tsv" | wc -l | tr -d ' ')
echo "$files files, $errors with a gap (not in the grid, error or panic): $out/formats.md"
