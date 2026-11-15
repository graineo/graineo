#!/bin/bash
# Starts graineo with the English UI (LANG_CODE=ja|fr|es for another language) in several views and collects painted texts without an English version
# (only with an own test folder; result: $D/audit.txt). Usage: scripts/i18n-screenshot-audit.sh [view ...]
D=${D:-/tmp/graineo-en-test}
IMG=${IMG:?set IMG to the path of an X3F file}  # BIN=path of the binary (default target/release/graineo)
VIEWS=${@:-library develop info edit filmlook look masks versions presets sensor crop align perspective about prefs darkroom darkroom-compare darkroom-tools darkroom-zones darkroom-border darkroom-dodge darkroom-recipes darkroom-contact darkroom-card darkroom-paper zoom export}
: > "$D/audit-raw.txt"; mkdir -p "$D/logs"; echo "{\"language\":\"${LANG_CODE:-en}\"}" > "$D/settings.json"
for i in $VIEWS; do
  t=${i%@*}; dlg=""; [ "$i" != "$t" ] && dlg=${i#*@}
  GRAINEO_DATA_DIR=$D GRAINEO_LIBRARY_NO_SYNC=1 GRAINEO_LIBRARY_SIDECARS=0 GRAINEO_NO_SPLASH=1 GRAINEO_TAB=$t GRAINEO_DIALOG=$dlg \
    GRAINEO_IMAGE=$IMG GRAINEO_I18N_AUDIT=1 GRAINEO_QUIT=${SECS:-7} "${BIN:-$(dirname "$0")/../target/release/graineo}" >/dev/null 2>&1
  grep -h "i18n-bild:" "$D"/logs/*.log | sed "s/^.*i18n-bild: //" | sed "s/^/$i\t/" >> "$D/audit-raw.txt"
  echo "done: $i"
done
cut -f2- "$D/audit-raw.txt" | sort -u > "$D/audit.txt"
wc -l "$D/audit.txt"
