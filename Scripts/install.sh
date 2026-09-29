#!/usr/bin/env bash
# install.sh — build and install speechnotes-linux for the current user.
# No root required: binary → ~/.local/bin, desktop entry + icon →
# ~/.local/share. Uninstall with scripts/uninstall.sh.
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$(pwd)"

step() { printf '\n== %s\n' "$1"; }

step "Build (release)"
if command -v swiftly >/dev/null 2>&1; then
    export PATH="$HOME/.local/share/swiftly/bin:$PATH"
elif [ -x "$HOME/.local/share/swiftly/bin/swift" ]; then
    export PATH="$HOME/.local/share/swiftly/bin:$PATH"
fi
swift build -c release
BIN=".build/out/Products/Release-linux-x86_64/speechnotes-linux"
[ -x "$BIN" ] || BIN="$(find .build -iname 'speechnotes-linux' -type f -path '*[Rr]elease*' | head -1)"
[ -n "$BIN" ] && [ -x "$BIN" ] || { echo "built binary not found"; exit 1; }

step "Install binary"
mkdir -p "$HOME/.local/bin"
# Atomic replace: a RUNNING app keeps its old inode; next launch gets this.
tmp="$HOME/.local/bin/speechnotes-linux.new"
cp "$BIN" "$tmp"
chmod +x "$tmp"
mv "$tmp" "$HOME/.local/bin/speechnotes-linux"

step "Install icon"
mkdir -p "$HOME/.local/share/icons/hicolor/256x256/apps"
cp config/icon.png "$HOME/.local/share/icons/hicolor/256x256/apps/speechnotes-linux.png"

step "Install desktop entry"
mkdir -p "$HOME/.local/share/applications"
cat > "$HOME/.local/share/applications/speechnotes-linux.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Speechnotes
GenericName=Notes with text-to-speech
Comment=Offline notes and books with high-quality TTS (Kokoro, Supertonic, Piper, Pico, eSpeak)
Exec=$HOME/.local/bin/speechnotes-linux
Icon=speechnotes-linux
Categories=Office;Utility;TextEditor;
Terminal=false
StartupWMClass=speechnotes-linux
EOF

# Optional extras the engines shell out to (already present on this box).
step "Optional native tools check"
for tool in ffmpeg ffprobe pdftotext pdfinfo pdftoppm pico2wave; do
    if command -v "$tool" >/dev/null 2>&1; then
        echo "  ok: $tool"
    else
        echo "  missing: $tool (books/PDF/pico features degrade without it)"
    fi
done

printf '\nInstalled. Launch from your app grid ("Speechnotes") or %s\n' \
    "$HOME/.local/bin/speechnotes-linux"
