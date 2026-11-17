#!/usr/bin/env bash
# Build the .deb from a release build + the bundled libraries.
# Layout (Debian conventions):
#   /usr/bin/speechnotes
#   /usr/lib/speechnotes/libonnxruntime.so, libpdfium.so   (private libs)
#   /usr/share/applications/io.github.thesagezm.Speechnotes.desktop
#   /usr/share/metainfo/io.github.thesagezm.Speechnotes.metainfo.xml
#   /usr/share/icons/hicolor/{128,256,512}x{...}/apps/speechnotes.png
#   /usr/share/man/man1/speechnotes.1
#   /usr/share/doc/speechnotes/{README.md, LICENSE, CHANGELOG.md}
# Depends are computed from ldd + the external programs the app shells out to.
# Usage: packaging/build-deb.sh [version]   (default: from bin/Cargo.toml)
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
version="${1:-$(grep -m1 '^version' "$root/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')}"
arch="$(dpkg --print-architecture)"
stage="$(mktemp -d)/speechnotes_${version}-${arch}"
trap 'rm -rf "$(dirname "$stage")"' EXIT

# --- tree -----------------------------------------------------------------
install -d "$stage/DEBIAN" \
    "$stage/usr/bin" "$stage/usr/lib/speechnotes" \
    "$stage/usr/share/applications" "$stage/usr/share/metainfo" \
    "$stage/usr/share/doc/speechnotes" "$stage/usr/share/man/man1"
for size in 128x128 256x256 512x512; do
    install -d "$stage/usr/share/icons/hicolor/$size/apps"
    install -m 644 "$root/assets/icons-src/$size/apps/speechnotes.png" \
        "$stage/usr/share/icons/hicolor/$size/apps/speechnotes.png"
done

install -m 755 "$root/target/release/speechnotes" "$stage/usr/bin/speechnotes"
# --- bundled libraries ----------------------------------------------------
# Fetched by packaging/fetch-libs.sh into $root/dist/lib-bundle/
if [ -d "$root/dist/lib-bundle" ]; then
    for lib in "$root"/dist/lib-bundle/*; do
        install -m 644 "$lib" "$stage/usr/lib/speechnotes/"
    done
else
    echo "warning: dist/lib-bundle empty — no onnxruntime/pdfium bundled" >&2
fi

install -m 644 "$root/packaging/io.github.thesagezm.Speechnotes.desktop" \
    "$stage/usr/share/applications/"
install -m 644 "$root/packaging/io.github.thesagezm.Speechnotes.metainfo.xml" \
    "$stage/usr/share/metainfo/"
gzip -9n -c "$root/packaging/man/speechnotes.1" \
    > "$stage/usr/share/man/man1/speechnotes.1.gz"
for f in README.md LICENSE CHANGELOG.md; do
    install -m 644 "$root/$f" "$stage/usr/share/doc/speechnotes/$f"
done
gzip -9n "$stage/usr/share/doc/speechnotes/CHANGELOG.md"
gzip -9n "$stage/usr/share/doc/speechnotes/README.md"

# --- Depends from ldd ------------------------------------------------------
# System GTK/WebKit stack comes from ldd on the binary; the tools the app
# shells out to are hard Depends (features degrade without them).
ldd_deps() {
    ldd "$1" | awk '$3 ~ /^\// {print $3}' | sort -u | while read -r so; do
        pkg="$(dpkg-query -S "$(basename "$so")" 2>/dev/null | head -1 | cut -d: -f1 || true)"
        case "$pkg" in
            ""|speechnotes|*-dev|*-doc|*-examples) ;;
            *) echo "$pkg" ;;
        esac
    done | sort -u | paste -sd, - | sed 's/,$//'
}
GTK_DEPS="$(ldd_deps "$root/target/release/speechnotes" || true)"
GTK_DEPS="${GTK_DEPS:-libgtk-4-1, libwebkitgtk-6.0-4, libasound2t64}"
cat > "$stage/DEBIAN/control" <<EOF
Package: speechnotes
Version: ${version}
Section: utils
Priority: optional
Architecture: ${arch}
Maintainer: thesagezm <thesagezm@users.noreply.github.com>
Depends: ${GTK_DEPS}, espeak-ng, ffmpeg, poppler-utils, pulseaudio-utils
Recommends: tesseract-ocr
Suggests: tesseract-ocr-eng
Description: Notes that read themselves aloud — and a book reader with the same voice
 Speechnotes pairs a plain-JSON, no-lock-in notes library (markdown editor,
 notebooks, search, tags) with a book reader (EPUB, PDF, MOBI/AZW3, FB2, RTF,
 HTML, slide decks) and a full local text-to-speech stack: espeak-ng plus
 four neural engines (Kokoro-82M, nano-edge-tts, Supertonic, Phonon) that
 download their models on demand.
 .
 BookDrop transfers files from a phone over the LAN (LocalSend-compatible,
 ports 53317-53327). Data lives under \$XDG_DATA_HOME/speechnotes as plain
 JSON, byte-compatible with the iOS app.
EOF

# --- contents + build -------------------------------------------------------
dpkg-deb --root-owner-group --build "$stage" \
    "$root/dist/speechnotes_${version}-${arch}.deb"
echo "built: dist/speechnotes_${version}-${arch}.deb"
dpkg-deb -I "$root/dist/speechnotes_${version}-${arch}.deb" | head -14
