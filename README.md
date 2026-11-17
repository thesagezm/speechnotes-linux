# Speechnotes for Linux

**A GTK4 notes app that reads to you.** Write notes, collect books, and
listen — on-device, offline, in your own voice settings.

Speechnotes pairs a plain-JSON, no-lock-in notes library (markdown editor,
notebooks, search, tags) with a book reader (EPUB, PDF, MOBI/AZW3, FB2, RTF,
HTML, slide decks) and a full text-to-speech stack: five engines up to an
82M-param neural model, running locally, reading any note or book with
follow-along highlighting.

It is built for people who *listen* to their reading — long sessions,
read-along focus work, ADHD-friendly Pomodoro runs — and for anyone who
wants their library in files they own, not a cloud account.

> **Status: alpha** (`0.1.0-alpha.1`). The core is solid and tested daily
> against real libraries; the edges (exotic formats, unusual PDFs) still
> surprise. Expect rough spots and please report them.

---

## Highlights

- **Notes** — markdown with math, code, tables; notebooks, bookmarks, tags,
  full-text search; `.docx` and Joplin `.jex` interchange
- **Read aloud** — espeak-ng (built in), Kokoro-82M, nano-edge-tts,
  Supertonic, Phonon; hot-switchable; speak-from-selection; per-note voice
  memory; the voice follows the page while you read along
- **Books** — EPUB as the publisher made it, PDF with OCR for scanned pages,
  MOBI/AZW3, FB2, RTF, HTML, PowerPoint/ODP decks with a presenter window
- **Audiobooks** — import `.m4b`/`.opus`/`.mp3`, generate `.wav` exports
- **BookDrop** — send files from your phone over the LAN
  (LocalSend-compatible, ports 53317–53327)
- **Focus** — Pomodoro with structured plans ("3 × 25 min", "finish by
  17:00"), tasks inside notes, chimes
- **Yours** — everything is local JSON (byte-compatible with the iOS app);
  no telemetry, no accounts, no cloud
- **E-Ink friendly** — System/Light/Dark/Warm/E-Ink schemes, full typography
  controls

Full list: [docs/FEATURES.md](docs/FEATURES.md) · On-disk format:
[docs/DATA-FORMAT.md](docs/DATA-FORMAT.md)

## Install

Downloads ship as a binary tarball (installer included), a `.deb`
(Ubuntu 24.04 / Debian 13 or newer), an AppImage, a Flatpak and a source
tarball — grab them from
[Releases](https://github.com/thesagezm/speechnotes-linux/releases).

```bash
# .deb
sudo apt install ./speechnotes_0.1.0-alpha.1_amd64.deb

# binary tarball (user-local install)
tar xzf speechnotes-0.1.0-alpha.1-linux-x86_64.tar.gz
cd speechnotes-0.1.0-alpha.1-linux-x86_64 && ./install.sh
```

The AppImage and Flatpak are self-contained. The tarball and `.deb` bundle
the neural inference libraries but expect a normal desktop system (GTK4,
WebKitGTK 6.0) and optionally use `espeak-ng`, `ffmpeg`, `poppler-utils` and
`tesseract-ocr` when installed.

Neural TTS engines download their model files from HuggingFace on first use
(a one-time, per-engine download of roughly 30–330 MB) — see the Models page
in the app. espeak works with no download at all.

## Build from source

```bash
# Debian/Ubuntu/Pop!_OS build dependencies
sudo apt install build-essential pkg-config rustc cargo \
    libgtk-4-dev libwebkit2gtk-6.0-dev libasound2-dev libglib2.0-dev

cargo build --release
./target/release/speechnotes --version
```

Requires Rust **1.85+** (2021 edition). The icon theme is compiled in via
`glib-compile-resources` (part of `libglib2.0-dev-bin`). Optional, for tidy
panic traces (the release CI does the same):

```bash
RUSTFLAGS="--remap-path-prefix=$HOME/.cargo=/cargo --remap-path-prefix=$PWD=/src" \
    cargo build --release
```

Runtime checks and
self-tests:

```bash
cargo test --workspace
./target/release/speechnotes --selftest
```

### Runtime dependencies

| Purpose | Program | Package |
| --- | --- | --- |
| Built-in TTS + phoneme extraction | `espeak-ng` | `espeak-ng` |
| Audio export / chime fallback | `ffmpeg`, `ffplay` | `ffmpeg` |
| Chime playback | `paplay` | `pulseaudio-utils` |
| PDF page rasterization | `pdftoppm` | `poppler-utils` |
| OCR backend (optional) | `tesseract` | `tesseract-ocr` |
| Open files / URLs | `xdg-open` | `xdg-utils` |
| System theme detection | `gsettings` | `glib2` / GNOME |

`onnxruntime` and `pdfium` are loaded at runtime; the release bundles place
them where the app looks first (system install: `/usr/lib/speechnotes/`;
user install: `~/.local/lib/speechnotes/`), with a fallback to
`LD_LIBRARY_PATH`/system copies.

### Networking

- Model downloads: `huggingface.co` (only when you install an engine)
- BookDrop: LAN TCP **53317–53327** (LocalSend protocol)

## CLI

The binary is fully scriptable:

```
speechnotes --selftest                 batch gate: run every core invariant
speechnotes --speak "text" --engine kokoro --voice af_heart
speechnotes --engines                  list the speech engines
speechnotes --receive                  headless BookDrop receiver
speechnotes --import-book book.epub    import into the library
speechnotes --import-note note.docx
speechnotes --books                    list the library
speechnotes --open-book <id> [page]    open a book
speechnotes --read-book <id>           extract a book's text
speechnotes --import-audiobook a.m4b
speechnotes --play-audiobook <id>
speechnotes --play-note <id>           synthesize a note to the default sink
speechnotes --version
```

## Data locations

```
$XDG_DATA_HOME/speechnotes/     (default ~/.local/share/speechnotes)
    notes.json, notebooks.json, bookmarks.json, Books/, models/, Exports/
$XDG_CONFIG_HOME/speechnotes/   (default ~/.config/speechnotes)
    prefs.json
```

Everything is plain JSON — see [docs/DATA-FORMAT.md](docs/DATA-FORMAT.md)
for the byte-level contract (including the Apple-epoch quirk that makes
libraries portable to and from the iOS app).

## Development notes

- `scripts/gui-drive.py` drives the running app over the accessibility tree
  (`python3 scripts/gui-drive.py map docs/UI-MAP.md` regenerates the UI map)
- `scripts/focused-probe.py` reports the focused widget chain (keyboard bug
  triage); `scripts/truepdf-ab-survey.py` scores the PDF text extractor
  against poppler
- Workspace layout: `bin/` (the `speechnotes` binary) plus `sn-core`,
  `sn-format`, `sn-media`, `sn-net`, `sn-store`, `sn-doc`, `sn-tts`, `sn-ui`

## Acknowledgements

Built on outstanding open work: [GTK4](https://gtk.org),
[WebKitGTK](https://webkitgtk.org),
[foliate-js](https://github.com/johnfactotum/foliate-js) (the paginator at
the heart of the book view), [Kokoro](https://huggingface.co/hexgrad/Kokoro-82M)
and its ONNX export,
[nano-edge-tts](https://huggingface.co/drmhse/nano-edge-tts),
[Supertonic](https://huggingface.co/Supertone/supertonic),
[phonon](https://huggingface.co/gradium/phonon-31m-en),
[RapidOCR](https://huggingface.co/SWHL/RapidOCR),
[DeckCraft](https://github.com/storytold/deckcraft),
[LocalSend](https://localsend.org) (protocol).

## License

[MIT](LICENSE)
