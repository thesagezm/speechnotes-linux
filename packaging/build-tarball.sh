#!/usr/bin/env bash
# Build the binary tarball: bin + bundled libs + icons + desktop + metainfo +
# manpage + install.sh/uninstall.sh + README/LICENSE/CHANGELOG.
# Usage: packaging/build-tarball.sh [version]
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
version="${1:-$(grep -m1 '^version' "$root/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')}"
name="speechnotes-${version}-linux-x86_64"
stage="$root/dist/tarball/$name"
rm -rf "$stage"
install -d "$stage/bin" "$stage/lib" \
    "$stage/share/applications" "$stage/share/metainfo" \
    "$stage/share/man/man1"

install -m 755 "$root/target/release/speechnotes" "$stage/bin/speechnotes"
if [ -d "$root/dist/lib-bundle" ]; then
    for lib in "$root"/dist/lib-bundle/*; do
        [ -e "$lib" ] || continue
        install -m 644 "$lib" "$stage/lib/"
    done
else
    echo "warning: no dist/lib-bundle — run packaging/fetch-libs.sh first" >&2
fi

for size in 128x128 256x256 512x512; do
    install -d "$stage/share/icons/hicolor/$size/apps"
    install -m 644 "$root/assets/icons-src/$size/apps/speechnotes.png" \
        "$stage/share/icons/hicolor/$size/apps/speechnotes.png"
done
install -m 644 "$root/packaging/io.github.thesagezm.Speechnotes.desktop" \
    "$stage/share/applications/"
install -m 644 "$root/packaging/io.github.thesagezm.Speechnotes.metainfo.xml" \
    "$stage/share/metainfo/"
install -m 644 "$root/packaging/man/speechnotes.1" "$stage/share/man/man1/"
for f in README.md LICENSE CHANGELOG.md; do
    install -m 644 "$root/$f" "$stage/$f"
done
install -m 755 "$root/packaging/install.sh" "$stage/install.sh"
install -m 755 "$root/packaging/uninstall.sh" "$stage/uninstall.sh"

tar -czf "$root/dist/${name}.tar.gz" -C "$root/dist/tarball" "$name"
rm -rf "$stage"
echo "built: dist/${name}.tar.gz"
sha256sum "$root/dist/${name}.tar.gz"
