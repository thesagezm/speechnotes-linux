// bootstrap.sh — one-time machine setup for speechnotes-linux.
//
// Idempotent: safe to run repeatedly. Needs no root.
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$(pwd)"

step() { printf '\n== %s\n' "$1"; }

step "Swift toolchain"
if command -v swiftly >/dev/null 2>&1; then
    echo "swiftly present: $(swiftly --version 2>/dev/null || true)"
else
    # swiftly itself may not be on PATH yet; look in its default home.
    export SWIFTLY_HOME_DIR="${SWIFTLY_HOME_DIR:-$HOME/.local/share/swiftly}"
    if [ ! -x "$SWIFTLY_HOME_DIR/bin/swiftly" ]; then
        echo "Installing swiftly..."
        curl -fsSL -O https://download.swift.org/swiftly/linux/swiftly-"$(uname -m)".tar.gz
        tar zxf swiftly-"$(uname -m)".tar.gz
        ./swiftly init --platform ubuntu24.04 --no-modify-profile --skip-install --assume-yes
    fi
    export PATH="$SWIFTLY_HOME_DIR/bin:$PATH"
fi
swift --version

step "espeak-ng linker symlink (no root available)"
mkdir -p "$HOME/.local/lib"
if [ ! -e "$HOME/.local/lib/libespeak-ng.so" ]; then
    ln -s /usr/lib/x86_64-linux-gnu/libespeak-ng.so.1 "$HOME/.local/lib/libespeak-ng.so"
    echo "linked ~/.local/lib/libespeak-ng.so"
else
    echo "already present"
fi

step "XDG data directories"
"$ROOT/.build/debug/alsa-tone" >/dev/null 2>&1 || true
mkdir -p "$HOME/.local/share/speechnotes" "$HOME/.cache/speechnotes" "$HOME/.config/speechnotes"
echo "ok"

step "Build"
swift build --product speechnotes-linux

step "Phase-0 gates"
swift build --product gtk-smoke --product alsa-tone --product espeak-say
echo "-- tone:"; ./.build/debug/alsa-tone | tail -2
echo "-- speech:"; ./.build/debug/espeak-say en-us "Bootstrap complete." | tail -2
echo "-- window:"; echo "(open manually with GDK_BACKEND=wayland ./.build/debug/gtk-smoke)"

step "Done"
echo "Run the app:  GDK_BACKEND=wayland ./.build/debug/speechnotes-linux"
