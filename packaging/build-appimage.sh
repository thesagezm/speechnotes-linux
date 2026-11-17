#!/usr/bin/env bash
# Build the AppImage. Two tiers of self-containment:
#   full  — bundles the GTK4/WebKit stack recursively (linuxdeploy), the
#           WebKit helper processes, and the GStreamer plugins WebKit needs
#           for <audio>/<video> inside books. Runs on distros without
#           webkitgtk-6.0.
# slim  — bundles only the private inference libraries (onnxruntime/pdfium);
#           needs webkitgtk-6.0 on the host (same systems as the .deb).
# Usage: packaging/build-appimage.sh [version] [full|slim]
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
version="${1:-$(grep -m1 '^version' "$root/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')}"
tier="${2:-slim}"
app_id="io.github.thesagezm.Speechnotes"
appdir="$root/dist/appimage/AppDir"
apptools="$root/dist/apptools"

rm -rf "$appdir"
mkdir -p "$appdir/usr/bin" "$appdir/usr/lib/speechnotes" \
    "$appdir/usr/share/applications" "$appdir/usr/share/metainfo" \
    "$appdir/usr/share/icons/hicolor/512x512/apps"

install -m 755 "$root/target/release/speechnotes" "$appdir/usr/bin/speechnotes"
install -m 644 "$root/packaging/$app_id.desktop" \
    "$appdir/usr/share/applications/$app_id.desktop"
# appimagetool looks for the desktop entry at the AppDir root too.
install -m 644 "$root/packaging/$app_id.desktop" "$appdir/$app_id.desktop"
install -m 644 "$root/packaging/$app_id.metainfo.xml" \
    "$appdir/usr/share/metainfo/$app_id.metainfo.xml"
for size in 128x128 256x256 512x512; do
    install -Dm644 "$root/assets/icons-src/$size/apps/speechnotes.png" \
        "$appdir/usr/share/icons/hicolor/$size/apps/speechnotes.png"
done
if [ -d "$root/dist/lib-bundle" ]; then
    for lib in "$root"/dist/lib-bundle/*; do
        [ -e "$lib" ] || continue
        install -m 644 "$lib" "$appdir/usr/lib/speechnotes/"
    done
fi

if [ "$tier" = "full" ]; then
    # Bundle the GTK/WebKit stack recursively. --deploy-deps-only keeps
    # linuxdeploy from mangling the desktop file.
    "$apptools/linuxdeploy/linuxdeploy.AppImage" \
        --appdir "$appdir" \
        --executable "$appdir/usr/bin/speechnotes" \
        --exclude-library "libonnxruntime.so" \
        --exclude-library "libpdfium.so" \
        --deploy-deps-only "$appdir/usr/bin/speechnotes"

    # WebKit's spawned helpers (WebProcess/NetworkProcess). The pkg-config
    # variable is unreliable — search the known Debian/Ubuntu locations.
    wk_helpers="$(find /usr/lib/x86_64-linux-gnu/webkitgtk-6.0 /usr/libexec \
        -maxdepth 3 \( -name 'WebKitWebProcess' -o -name 'WebKitNetworkProcess' \) \
        2>/dev/null | head -2 || true)"
    for helper in $wk_helpers; do
        install -Dm755 "$helper" \
            "$appdir/usr/libexec/webkitgtk-6.0/$(basename "$helper")"
    done
    "$apptools/linuxdeploy/linuxdeploy.AppImage" \
        --appdir "$appdir" \
        --deploy-deps-only "$appdir/usr/libexec/webkitgtk-6.0/WebKitWebProcess" || true
    "$apptools/linuxdeploy/linuxdeploy.AppImage" \
        --appdir "$appdir" \
        --deploy-deps-only "$appdir/usr/libexec/webkitgtk-6.0/WebKitNetworkProcess" || true

    # GStreamer plugins WebKit needs for media inside books.
    gst_plugins="$(pkg-config --variable=pluginsdir gstreamer-1.0 2>/dev/null || echo /usr/lib/x86_64-linux-gnu/gstreamer-1.0)"
    if [ -d "$gst_plugins" ]; then
        mkdir -p "$appdir/usr/lib/gstreamer-1.0"
        cp -a "$gst_plugins/." "$appdir/usr/lib/gstreamer-1.0/"
    fi
    # GLib schemas: ship the system schemas WebKit/GTK apps expect.
    if [ -d /usr/share/glib-2.0/schemas ]; then
        mkdir -p "$appdir/usr/share/glib-2.0/schemas"
        cp -a /usr/share/glib-2.0/schemas/. "$appdir/usr/share/glib-2.0/schemas/"
        glib-compile-schemas "$appdir/usr/share/glib-2.0/schemas" || true
    fi
fi

# --- AppRun wrapper ----------------------------------------------------------
# linuxdeploy installs AppRun as a symlink to the binary when it deploys the
# desktop file; remove it first so the wrapper is a real script (writing
# through the symlink would truncate the binary).
rm -f "$appdir/AppRun"
cat > "$appdir/AppRun" <<'EOF'
#!/usr/bin/env bash
# AppRun: point the loaders and WebKit at the bundled tree. The app's own
# dylib lookup also checks $APPDIR/usr/lib/speechnotes via /usr paths being
# remapped here for onnxruntime/pdfium.
HERE="$(cd "$(dirname "$(readlink -f "$0")")" && pwd)"
export APPDIR="$HERE"
export LD_LIBRARY_PATH="$HERE/usr/lib:$HERE/usr/lib/speechnotes:${LD_LIBRARY_PATH:-}"
if [ -x "$HERE/usr/libexec/webkitgtk-6.0/WebKitWebProcess" ]; then
    export WEBKIT_EXEC_PATH="$HERE/usr/libexec/webkitgtk-6.0"
    export WEBKIT_FORCE_SANDBOX=0     # the bwrap sandbox cannot see the AppDir
fi
if [ -d "$HERE/usr/lib/gstreamer-1.0" ]; then
    export GST_PLUGIN_PATH="$HERE/usr/lib/gstreamer-1.0${GST_PLUGIN_PATH:+:$GST_PLUGIN_PATH}"
    export GST_PLUGIN_SYSTEM_PATH="$HERE/usr/lib/gstreamer-1.0"
fi
if [ -d "$HERE/usr/share/glib-2.0/schemas" ]; then
    export GSETTINGS_SCHEMA_DIR="$HERE/usr/share/glib-2.0/schemas"
fi
exec "$HERE/usr/bin/speechnotes" "$@"
EOF
chmod +x "$appdir/AppRun"

mkdir -p "$root/dist/appimage/out"
cd "$root/dist/appimage"
"$apptools/appimagetool/appimagetool.AppImage" -n "$appdir" \
    "out/Speechnotes-${version}-x86_64.AppImage"
echo "built: dist/appimage/out/Speechnotes-${version}-x86_64.AppImage"
sha256sum "out/Speechnotes-${version}-x86_64.AppImage"
