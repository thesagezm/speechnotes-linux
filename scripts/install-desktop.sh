#!/usr/bin/env bash
# Desktop integration for a user-local Speechnotes install:
#   - the app icon (the iOS/Android rounded-corner mark) into the user's
#     hicolor icon theme, so the launcher resolves it without the app running;
#   - the desktop entry under the GApplication id (APP_ID), so a Wayland
#     shell matches the running app to the entry by app_id and shows the
#     same icon in the dock. The pre-APP_ID file (speechnotes-linux.desktop)
#     is removed when present — its entry would duplicate this one.
# Run after moving the binary into ~/.local/bin (the entry's Exec).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
bindir="${BINDIR:-$HOME/.local/bin}"
icons_dir="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor"
apps_dir="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
app_id="io.github.thesagezm.Speechnotes"

for size in 128x128 256x256 512x512; do
    target="$icons_dir/$size/apps"
    mkdir -p "$target"
    cp "$root/assets/icons-src/$size/apps/speechnotes.png" "$target/"
done

mkdir -p "$apps_dir"
cat > "$apps_dir/$app_id.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Speechnotes
GenericName=Notes and text-to-speech
Comment=Write notes, books and audiobooks, and listen to them
Exec=$bindir/speechnotes
Icon=speechnotes
Terminal=false
Categories=Utility;TextEditor;AudioVideo;Audio;
Keywords=notes;text;speech;tts;audiobook;epub;reader;
StartupNotify=true
StartupWMClass=Speechnotes
EOF

# The pre-APP_ID entry name; a leftover means two launchers for one app.
old="$apps_dir/speechnotes-linux.desktop"
if [ -f "$old" ]; then
    rm -f "$old"
    echo "removed $old (superseded by the APP_ID entry)"
fi

gtk-update-icon-cache -f -t "$icons_dir" 2>/dev/null || true
echo "installed: hicolor icons + $app_id.desktop"
