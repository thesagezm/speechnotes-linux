# Flatpak build notes

The manifest references `cargo-sources.json` (the Cargo.lock → flatpak
source list) and needs real sha256 pins. One-time setup:

```bash
# 1. Generate the cargo sources from Cargo.lock
git clone https://github.com/flatpak/flatpak-builder-tools
pip install toml
python3 flatpak-builder-tools/cargo/flatpak-cargo-generator.py \
    Cargo.lock -o packaging/flatpak/cargo-sources.json

# 2. Replace the placeholder sha256 values in the manifest. The fetch-libs
#    script prints the two library hashes:
packaging/fetch-libs.sh
#   → copy the printed sha256 values into io.github.thesagezm.Speechnotes.yml
#   → the espeak-ng/poppler/ffmpeg/tesseract hashes come from their release
#     pages (`sha256sum` after a manual download, then pin)

# 3. Build (installs the Rust SDK extension on first use)
flatpak install --user flathub org.gnome.Sdk//48 org.gnome.Platform//48 \
    org.freedesktop.Sdk.Extension.rust-stable//48
flatpak-builder --force-clean --install --user build \
    packaging/flatpak/io.github.thesagezm.Speechnotes.yml

# 4. Smoke test
flatpak run io.github.thesagezm.Speechnotes --version
```

Inside the sandbox, `$XDG_DATA_HOME` is the sandbox home, so the library
lands in `~/.var/app/io.github.thesagezm.Speechnotes/data/speechnotes/`.
The bundled libraries land in `/app/lib` via the onnxruntime/pdfium modules;
the app's lookup order checks `/usr/lib/speechnotes/` first — for the
flatpak build, set `SPEECHNOTES_ORT_LIB`-style overrides or symlink the
module output into `/app/lib/speechnotes/` (the manifest's `build-commands`
can be extended to do that with `mkdir -p /app/lib/speechnotes && cp`).
