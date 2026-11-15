#!/bin/bash
# Screenshots of all views and dialogs (own test folder, own windows only).
# Usage: scripts/view-screenshots.sh <output_folder> [de|en] [view ...]
# View: tab (e.g. darkroom, prefs:Export) or tab@dialog (e.g. library@delete-selection).
OUT=${1:?output folder}; LANG_SEL=${2:-de}; shift 2
D=${D:-/tmp/graineo-photo-data}
IMG=${IMG:?set IMG to the path of an X3F file}
mkdir -p "$OUT" "$D"
echo "{\"language\":\"$LANG_SEL\"}" > "$D/settings.json"
VIEWS=${@:-library develop info edit filmlook look masks versions presets sensor crop align perspective about export zoom darkroom darkroom-compare darkroom-tools darkroom-zones darkroom-border darkroom-dodge darkroom-recipes darkroom-contact darkroom-card darkroom-paper prefs:Allgemein prefs:Oberfläche prefs:Bibliothek prefs:Entwickeln prefs:Export prefs:Objektive prefs:Suche prefs:Module prefs:Speicher prefs:Looks library@delete-selection library@export library@copy library@shortcuts library@import library@smart library@lightroom library@rebuild library@rename library@error library@keyword-set library@lens-wizard develop@export develop@copy develop@delete develop@shortcuts}
for i in $VIEWS; do
  t=${i%@*}; dlg=""; [ "$i" != "$t" ] && dlg=${i#*@}
  n=$(echo "$i" | tr ':@' '--')
  GRAINEO_DATA_DIR=$D GRAINEO_LIBRARY_NO_SYNC=1 GRAINEO_LIBRARY_SIDECARS=0 GRAINEO_NO_SPLASH=1 GRAINEO_TAB=$t GRAINEO_DIALOG=$dlg \
    GRAINEO_IMAGE=$IMG GRAINEO_SCREENSHOT="$OUT/$LANG_SEL-$n.png@${SECS:-9}" GRAINEO_QUIT=$(( ${SECS:-9} + 3 )) "$(dirname "$0")/../target/release/graineo" >/dev/null 2>&1
  echo "done: $i"
done
