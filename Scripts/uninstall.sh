#!/usr/bin/env bash
# uninstall.sh — remove the user-local speechnotes-linux install.
# Does NOT touch your data (~/.local/share/speechnotes).
set -euo pipefail

rm -f "$HOME/.local/bin/speechnotes-linux"
rm -f "$HOME/.local/share/applications/speechnotes-linux.desktop"
rm -f "$HOME/.local/share/icons/hicolor/256x256/apps/speechnotes-linux.png"

printf 'Removed. Your notes and books are untouched at:\n  %s\n' \
    "$HOME/.local/share/speechnotes"
