#!/usr/bin/env bash
# User-local install for the Speechnotes binary tarball:
#   ~/.local/bin/speechnotes           the app
#   ~/.local/lib/speechnotes/          bundled onnxruntime + pdfium
#   ~/.local/share/icons/hicolor/...   launcher icons
#   ~/.local/share/applications/       desktop entry (APP_ID-matched)
#   ~/.local/share/metainfo/           AppStream metadata
#   ~/.local/share/man/man1/           manpage
# Run from the extracted tarball root. Honours PREFIX-style overrides:
#   BINDIR=... LIBDIR=... ./install.sh
set -euo pipefail
root="$(cd "$(dirname "$0")" && pwd)"
bindir="${BINDIR:-$HOME/.local/bin}"
libdir="${LIBDIR:-$HOME/.local/lib/speechnotes}"
datadir="${XDG_DATA_HOME:-$HOME/.local/share}"
app_id="io.github.thesagezm.Speechnotes"

mkdir -p "$bindir" "$libdir" "$datadir/applications" "$datadir/metainfo" \
         "$datadir/man/man1" "$datadir/icons/hicolor/128x128/apps" \
         "$datadir/icons/hicolor/256x256/apps" \
         "$datadir/icons/hicolor/512x512/apps"

install -m 755 "$root/bin/speechnotes" "$bindir/speechnotes"
for lib in "$root"/lib/*; do
    [ -e "$lib" ] || continue
    install -m 644 "$lib" "$libdir/"
done

for size in 128x128 256x256 512x512; do
    install -m 644 "$root/share/icons/hicolor/$size/apps/speechnotes.png" \
        "$datadir/icons/hicolor/$size/apps/speechnotes.png"
done
install -m 644 "$root/share/applications/$app_id.desktop" \
    "$datadir/applications/$app_id.desktop"
install -m 644 "$root/share/metainfo/$app_id.metainfo.xml" \
    "$datadir/metainfo/$app_id.metainfo.xml"
install -m 644 "$root/share/man/man1/speechnotes.1" \
    "$datadir/man/man1/speechnotes.1"

gtk-update-icon-cache -f -t "$datadir/icons/hicolor" 2>/dev/null || true
update-desktop-database "$datadir/applications" 2>/dev/null || true

echo "installed:"
echo "  $bindir/speechnotes"
echo "  $libdir/ (bundled inference libraries)"
echo "  desktop entry: $app_id.desktop"
echo
echo "Optional runtime helpers when available: espeak-ng, ffmpeg, poppler-utils,"
echo "tesseract-ocr, pulseaudio-utils."
