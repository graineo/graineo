#!/bin/sh
# Installs the experimental Linux build for the current user (no root needed):
#   program, graineo-dev, film data and licences -> ~/.local/share/graineo/app/
#   symlink ~/.local/bin/graineo, desktop entry and icon for the application menu.
# ./install.sh --uninstall removes exactly these files; photos, library, settings and caches are never touched.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
DATA=${XDG_DATA_HOME:-$HOME/.local/share}
APP="$DATA/graineo/app"
BIN="$HOME/.local/bin"
DESKTOP="$DATA/applications/graineo.desktop"
ICON="$DATA/icons/hicolor/1024x1024/apps/graineo.png"

refresh() {
  command -v update-desktop-database > /dev/null 2>&1 && update-desktop-database -q "$DATA/applications" || true
  command -v gtk-update-icon-cache > /dev/null 2>&1 && gtk-update-icon-cache -q -t "$DATA/icons/hicolor" 2> /dev/null || true
}

if [ "${1:-}" = "--uninstall" ]; then
  # only what install.sh created; the user data next to app/ (library, previews, settings ...) stay
  if [ -L "$BIN/graineo" ] && [ "$(readlink "$BIN/graineo")" = "$APP/graineo" ]; then rm -f "$BIN/graineo"; fi
  rm -f "$DESKTOP" "$ICON"
  for f in graineo graineo-dev graineo.png LICENSE README-linux.txt install.sh; do rm -f "$APP/$f"; done
  rm -rf "$APP/spektrafilm" "$APP/licenses"
  rmdir "$APP" 2> /dev/null || true
  refresh
  echo "graineo removed (your library and settings in $DATA/graineo were kept)."
  exit 0
fi

[ -x "$HERE/graineo" ] || { echo "install.sh: run it from the unpacked graineo folder" >&2; exit 1; }
mkdir -p "$APP" "$BIN" "$(dirname "$DESKTOP")" "$(dirname "$ICON")"
for f in graineo graineo-dev graineo.png LICENSE README-linux.txt install.sh; do
  [ -e "$HERE/$f" ] && cp -f "$HERE/$f" "$APP/$f"
done
rm -rf "$APP/spektrafilm" "$APP/licenses"
cp -R "$HERE/spektrafilm" "$HERE/licenses" "$APP/"
ln -sfn "$APP/graineo" "$BIN/graineo"
cp -f "$HERE/graineo.png" "$ICON"
sed "s|^Exec=.*|Exec=\"$APP/graineo\" %F|; s|^Icon=.*|Icon=graineo|" "$HERE/graineo.desktop" > "$DESKTOP"
refresh
echo "graineo installed in $APP"
echo "Start: graineo (from the application menu, or in a terminal if ~/.local/bin is in PATH)"
echo "Remove: $APP/install.sh --uninstall"
