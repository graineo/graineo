#!/bin/bash
# Regression run: dragging the zoomed image must pan it (single view, comparisons, darkroom).
# Starts graineo with its own test folder, zooms to 100 % (Z), drags with real pointer events (GRAINEO_INPUT drag:…)
# and checks the logged pan (GRAINEO_ZOOM_TEST report). Never touches the real library.
# Usage: scripts/drag-test.sh <photo> [<photo> …]   (BIN=… for another build, D=… for the test folder)
BIN=${BIN:-"$(dirname "$0")/../target/release/graineo"}
D=${D:-/tmp/graineo-drag-test}
[ $# -gt 0 ] || { echo "usage: $0 <photo> [<photo> …]"; exit 2; }
fail=0
reports=$(seq -s, -f "report@%g" 1 0.5 30)
for img in "$@"; do
  for view in develop compare unedited darkroom; do
    tab=$view; before=""
    case $view in
      compare) tab=develop; before="view=compare" ;;
      unedited) tab=develop; before="view=before" ;;
    esac
    rm -rf "$D"; mkdir -p "$D"
    # the drag starts right of the centre (not on a comparison divider) and moves 200 pt left, 100 pt up
    GRAINEO_DATA_DIR=$D GRAINEO_LIBRARY_NO_SYNC=1 GRAINEO_LIBRARY_SIDECARS=0 GRAINEO_NO_PHOTOKIT=1 GRAINEO_FACE_OFF=1 \
      GRAINEO_NO_SPLASH=1 GRAINEO_TAB=$tab GRAINEO_BEFORE_TEST=$before GRAINEO_ZOOM_TEST="$reports" \
      GRAINEO_INPUT="key:Z@12,drag:700:300:500:200@15" GRAINEO_QUIT=19 "$BIN" "$img" >/dev/null 2>&1
    zoomed=$(grep "report zoom" "$D/logs/trace.log" | grep -v "zoom 1.000" | head -1)
    last=$(grep "report zoom" "$D/logs/trace.log" | tail -1)
    if [ -z "$zoomed" ]; then
      echo "FAIL $view $(basename "$img"): not zoomed"; fail=1
    elif ! grep -q "pan -200.0 -100.0" <<<"$last"; then
      echo "FAIL $view $(basename "$img"): ${last#*report }"; fail=1
    else
      echo "ok   $view $(basename "$img"): ${last#*report }"
    fi
  done
done
exit $fail
