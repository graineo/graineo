#!/bin/bash
# Speed and image comparison against the 0.86.2 state.
#
# Images: scripts/perf-baseline.sh images <graineo-dev new> <reference_folder> <output> <files …>
#         computes the test set with the current state and compares it with the reference images of v0.86.2
#         (made with `graineo-dev perf-reference` on v0.86.2). Expected: all bit-identical.
# Speed:  scripts/perf-baseline.sh speed <program 0.86.2+test aid> <program new> <photo_folder> <output> [passes=3]
#         Runs alternate (old, new, old, new …) with scripts/perf-baseline-run.sh; evaluation per run as JSON.
#         Limit: visible times at most 10 % worse, otherwise stop and ask.
set -u
DIR=$(cd "$(dirname "$0")" && pwd)
mode=${1:?images|speed}; shift
case "$mode" in
images)
  DEV=${1:?graineo-dev}; REF=${2:?reference folder}; OUT=${3:?output}; shift 3
  "$DEV" perf baseline-compare "$REF" "$OUT" "$@" ;;
speed)
  OLD=${1:?old program}; NEW=${2:?new program}; PHOTOS=${3:?photo folder}; OUT=${4:?output}; N=${5:-3}
  mkdir -p "$OUT"
  for i in $(seq 1 "$N"); do
    for state in old new; do
      bin=$OLD; [ "$state" = new ] && bin=$NEW
      BIN=$bin "$DIR/perf-baseline-run.sh" "$OUT/$state-$i" "$PHOTOS" >/dev/null 2>&1
      python3 "$DIR/perf-evaluate.py" "$OUT/$state-$i" "$OUT/$state-$i.json" >/dev/null 2>&1
      echo "$state-$i: exit $(cat "$OUT/$state-$i/warm/exitcode.txt" 2>/dev/null), load $(uptime | sed 's/.*averages: //')"
    done
  done ;;
*) echo "unknown mode $mode"; exit 2 ;;
esac
