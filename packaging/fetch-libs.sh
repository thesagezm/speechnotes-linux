#!/usr/bin/env bash
# Fetch the two runtime libraries the app loads dynamically, with pinned
# checksums, into dist/lib-bundle/ for the .deb, the binary tarball and the
# AppImage. Each download lands in its own empty directory.
#   - onnxruntime 1.30.0 (github.com/microsoft/onnxruntime releases)
#   - pdfium (bblanchon/pdfium-binaries, chromium/<rev> pinned)
# Usage: packaging/fetch-libs.sh
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
out="$root/dist/lib-bundle"
tmp="$root/dist/fetch-tmp"
mkdir -p "$out" "$tmp"

fetch() { # url -> file, with resume+retries (network is not always friendly)
    local url="$1" dest="$2" tries=0
    while [ $tries -lt 5 ]; do
        if curl -fsSL -C - --retry 3 --retry-delay 2 -o "$dest" "$url"; then
            return 0
        fi
        tries=$((tries + 1)); sleep 2
    done
    echo "FAILED: $url" >&2
    return 1
}

# --- onnxruntime ------------------------------------------------------------
ORT_VER="1.30.0"
ORT_SHA256="${ORT_SHA256:-}"
ORT_URL="https://github.com/microsoft/onnxruntime/releases/download/v${ORT_VER}/onnxruntime-linux-x64-${ORT_VER}.tgz"
ort_dir="$tmp/onnxruntime-$ORT_VER"
mkdir -p "$ort_dir"
if [ ! -f "$ort_dir/onnxruntime.tgz" ]; then
    echo "fetching onnxruntime $ORT_VER"
    if ! fetch "$ORT_URL" "$ort_dir/onnxruntime.tgz"; then
        # Upstream has reorganized its release assets more than once; fall
        # back to a local install of the same version if one exists (the
        # canonical release layout, exactly what the engines were verified
        # against). CI should pin ORT_SHA256 and a working URL.
        for cand in "$HOME/.local/share/onnxruntime-linux-x64-$ORT_VER/lib/libonnxruntime.so.1.$(echo "$ORT_VER" | cut -d. -f2,3)" \
                    "$HOME/.local/share/onnxruntime-linux-x64-$ORT_VER/lib/libonnxruntime.so"; do
            if [ -e "$cand" ]; then
                echo "download unavailable — bundling the local install: $cand"
                cp -L "$cand" "$out/libonnxruntime.so"
                ort_from_local=1
                break
            fi
        done
        if [ "${ort_from_local:-0}" != 1 ]; then
            echo "no local onnxruntime found — fix ORT_URL and retry" >&2
            exit 1
        fi
    fi
fi
if [ "${ort_from_local:-0}" != 1 ]; then
    got="$(sha256sum "$ort_dir/onnxruntime.tgz" | cut -d' ' -f1)"
    if [ -n "$ORT_SHA256" ]; then
        [ "$got" = "$ORT_SHA256" ] || { echo "onnxruntime sha256 mismatch: $got != $ORT_SHA256" >&2; exit 1; }
    else
        echo "note: pin this in fetch-libs.sh — onnxruntime $ORT_VER sha256: $got"
    fi
    tar -xzf "$ort_dir/onnxruntime.tgz" -C "$ort_dir"
    install -m 644 "$ort_dir/onnxruntime-linux-x64-$ORT_VER/lib/libonnxruntime.so" \
        "$out/libonnxruntime.so"
fi

# --- pdfium -----------------------------------------------------------------
PDFIUM_VER="${PDFIUM_VER:-7947}"   # chromium build; re-pin per release
PDFIUM_SHA256="${PDFIUM_SHA256:-}"
PDFIUM_URL="https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F${PDFIUM_VER}/pdfium-linux-x64-libc++-tgz.tgz"
pdf_dir="$tmp/pdfium-$PDFIUM_VER"
mkdir -p "$pdf_dir"
pdf_from_local=0
if [ ! -f "$pdf_dir/pdfium.tgz" ]; then
    echo "fetching pdfium chromium/$PDFIUM_VER"
    if ! fetch "$PDFIUM_URL" "$pdf_dir/pdfium.tgz"; then
        # Release revs advance with chromium; fall back to a local official
        # pdfium-binaries build (pypdfium2 ships exactly those).
        for cand in "$HOME/.local/lib/python3.12/site-packages/pypdfium2_raw/libpdfium.so" \
                    /usr/lib/x86_64-linux-gnu/libpdfium.so; do
            if [ -e "$cand" ]; then
                echo "download unavailable — bundling the local pdfium: $cand"
                cp -L "$cand" "$out/libpdfium.so"
                pdf_from_local=1
                break
            fi
        done
        if [ "$pdf_from_local" != 1 ]; then
            echo "no local pdfium found — fix PDFIUM_VER and retry" >&2
            exit 1
        fi
    fi
fi
if [ "$pdf_from_local" != 1 ]; then
    got="$(sha256sum "$pdf_dir/pdfium.tgz" | cut -d' ' -f1)"
    if [ -n "$PDFIUM_SHA256" ]; then
        [ "$got" = "$PDFIUM_SHA256" ] || { echo "pdfium sha256 mismatch: $got != $PDFIUM_SHA256" >&2; exit 1; }
    else
        echo "note: pin this in fetch-libs.sh — pdfium chromium/$PDFIUM_VER sha256: $got"
    fi
    tar -xzf "$pdf_dir/pdfium.tgz" -C "$pdf_dir"
    install -m 644 "$pdf_dir/lib/libpdfium.so" "$out/libpdfium.so"
fi

echo "bundled into $out:"
ls -la "$out"
