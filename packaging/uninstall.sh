#!/usr/bin/env bash
# Reverse of install.sh (user-local install).
set -euo pipefail
bindir="${BINDIR:-$HOME/.local/bin}"
libdir="${LIBDIR:-$HOME/.local/lib/speechnotes}"
datadir="${XDG_DATA_HOME:-$HOME/.local/share}"
app_id="io.github.thesagezm.Speechnotes"

rm -f "$bindir/speechnotes"
rm -rf "$libdir"
rm -f "$datadir/applications/$app_id.desktop"
rm -f "$datadir/metainfo/$app_id.metainfo.xml"
rm -f "$datadir/man/man1/speechnotes.1"
for size in 128x128 256x256 512x512; do
    rm -f "$datadir/icons/hicolor/$size/apps/speechnotes.png"
done
gtk-update-icon-cache -f -t "$datadir/icons/hicolor" 2>/dev/null || true
update-desktop-database "$datadir/applications" 2>/dev/null || true
echo "removed: binary, bundled libraries, desktop entry, icons, manpage"
echo "(your library data under \$XDG_DATA_HOME/speechnotes is untouched)"
