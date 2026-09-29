# Speechnotes-Linux

A native Linux desktop notes app that reads your notes aloud, offline — the
**master version** of the Speechnotes family (iOS, Android, Linux). Swift +
GTK4 (via SwiftCrossUI), with the multi-engine TTS architecture used by
[Speech Note (dsnote)](https://github.com/mkiol/dsnote) — an abstract engine
contract, pluggable backends, and a model manager that downloads voices on
demand.

Personal project. **GPL-3.0** (espeak-ng and piper are GPL-3.0; SwiftCrossUI
and ONNX Runtime are MIT and fine as libraries).

## What it does

- **Notes** — notebooks, search, sort, pin/star, recycle bin, debounced
  saves with a rolling backup; notes.json is byte-compatible with the iOS
  app so a library moves between family members untouched.
- **Read aloud** — read-along strip (the sounding sentence highlights),
  per-note resume bookmarks with a one-tap auto-resume banner, speed 0.5–2×.
- **Books** — an EPUB/PDF/audiobook shelf:
  - EPUB: spine + TOC snapshotted at import, chapter text extracted
    straight from the archive (no webview), cover art, speak-per-chapter.
  - PDF: metadata + page-range chapters via poppler, page-1 cover,
    extracted text reader.
  - Audiobooks (M4B/M4A/MP4/MP3): chapters read from the file's own
    metadata (chpl/ID3 CHAP), streaming playback via ffmpeg → ALSA with
    per-chapter second bookmarks, auto-advance, and resume.
- **Export/import** — Joplin-compatible JEX round-trip (notebooks merge,
  ids preserved, re-import never duplicates), note → WAV render, per-note
  bookmarks.
- **Everything offline.** Only the model downloads touch the network.

## Engines

| Engine | Model | Size | Quality |
|---|---|---|---|
| eSpeak-NG | none | 0 | robotic, instant, 131 voices |
| Pico2Wave | distro CLI | 0 | light, 6 languages |
| Piper (VITS) | `.onnx` + `.onnx.json` | 20–130 MB | natural neural |
| Kokoro-82M | ONNX (uint8/fp32) | 86–326 MB | excellent, 28 voices |
| Supertonic | 4-stage ONNX | ~262 MB | excellent, 44.1 kHz |

Each engine implements four hooks — `model_created()`,
`model_supports_speed()`, `create_model()`, `encode_speech_impl(text, speed,
out_file)` — and the base class owns the worker thread, the task queue, the
state machine, and sentence splitting. Speech-to-text is deliberately **not**
part of this app: TTS is the mission.

Audiobook playback shells the box's `ffmpeg`; PDF books use `pdftotext`/
`pdfinfo`/`pdftoppm` (poppler-utils); Pico uses the distro's `pico2wave`.
All optional — features degrade with a log line, never a crash.

## Install

```sh
Scripts/install.sh      # build (release) + binary, icon and desktop entry → ~/.local
Scripts/uninstall.sh    # remove those; your data stays
```

Launch **Speechnotes** from the app grid, or `~/.local/bin/speechnotes-linux`.
Models download in Settings (Piper voice, Kokoro, Supertonic).

## Build & develop

```sh
Scripts/bootstrap.sh                      # one-time: toolchain, symlink, build, gates
swift build && swift test                 # the whole suite, no device needed
GDK_BACKEND=wayland ./.build/debug/speechnotes-linux
```

Phase-0 diagnostics:

```sh
./.build/debug/gtk-smoke                  # a raw GTK4 window (self-closing after 6s)
./.build/debug/alsa-tone                  # a 440 Hz tone
./.build/debug/espeak-say en-us "hi"      # one spoken sentence
```

CI: every push builds and runs the suite on ubuntu-24.04 (Build & Test);
tagging `v*` (or dispatching the Release workflow) publishes a versioned
tarball with binary, icon, desktop entry and installer to GitHub Releases.

## Machine notes (why the code looks the way it does)

- **No root.** Everything installs under `$HOME`. The one system gap:
  `libespeak-ng.so` (the unversioned linker name) doesn't exist without the
  `-dev` package, so `bootstrap.sh` symlinks it in `~/.local/lib`.
- **Swift via swiftly.** SwiftUI's idioms survive: SwiftCrossUI's
  `@State`/`@Environment`/`ObservableObject` map cleanly onto the iOS app's
  view code, which is what makes the three-app family (iOS/Android/Linux)
  maintainable by one person.
- **ONNX Runtime C API**: `CreateTensorWithDataAsOrtValue` does NOT copy the
  caller's buffer — `OrtValueRef` owns a private byte copy for its lifetime
  (a scoped Swift array's storage can be freed by ARC before `Run` reads it,
  which surfaces as nondeterministic garbage shapes inside the graph).
- **GTK4 quirks that cost real time** (documented so future-you doesn't
  repeat the archaeology):
  - `gtk_window_new()` returns `GtkWidget*` while `gtk_window_set_title()`
    takes `GtkWindow*`; Swift imports these as distinct types, so calls
    reinterpret the pointer (`OpaquePointer` round-trip).
  - GTK4 removed `gtk_main()`; use `g_main_loop_new`/`g_main_loop_run`.
  - ALSA's `snd_pcm_hw_params_malloc` out-parameter imports as a plain
    `OpaquePointer?` slot, not a named struct type.
  - `GtkWindow.list_toplevels()` is process-local — verifying a window is on
    screen cross-process needs `WAYLAND_DEBUG=1` (look for
    `xdg_toplevel.configure` + `wl_surface.commit`) or a screenshot diff.
- **Swift 6 strict concurrency**: ported files use `nonisolated(unsafe)` on
  lock-guarded statics rather than restructuring the iOS code.

## Layout

```
Sources/
  SpeechLogic/     pure logic ported from speechnotes-ios (SentenceChunker,
                   SpeechSanitizer, MarkdownText, ZipReader→zlib, EpubInfo,
                   XhtmlText, WAVWriter, JexExport/JexImport, AudiobookChapters,
                   PdfChapters, …)
  EspeakBridge/    Swift bridge over libespeak-ng (retrieval mode)
  AlsaSink/        ALSA playback + position clock
  AppPaths/        XDG data/cache/config roots
  Log/             ring buffer + on-disk tail
  TTSEngine/       the tier: engine base + factory, Espeak/Pico/Piper/Kokoro/
                   Supertonic engines, TTS + audiobook players and controllers,
                   ONNX Runtime C API wrappers
  Data/            NotesStore, NotebooksStore, BooksStore, Prefs, BookmarkStore,
                   JexImporter — byte-compatible with the iOS app's files
  SpeechnotesLinux/ the GTK app (shell, notes list, editor, books shelf,
                   reader, recycle bin, settings)
Tests/             SpeechLogic, Data, TTS engine suites (offline; model-backed
                   tests skip unless the model is installed)
```

## Credits & licenses

- [Speech Note (dsnote)](https://github.com/mkiol/dsnote) — MPL-2.0; the
  engine architecture (four-virtual `tts_engine` contract, per-chunk WAV
  handoff, JSON model catalog) follows its design.
- [espeak-ng](https://github.com/espeak-ng/espeak-ng) — GPL-3.0.
- [SwiftCrossUI](https://github.com/stackotter/swift-cross-ui) — MIT.
- [Kokoro](https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX) —
  model Apache-2.0; [misaki](https://github.com/hexgrad/misaki) G2P
  Apache-2.0.
- [Supertonic](https://huggingface.co/supertone-oss-archive/supertonic) —
  code MIT, model OpenRAIL-M.
- Piper voices — `rhasspy/piper-voices`, per-voice MODEL_CARD licenses.
- Ported code from [speechnotes-ios](../speechnotes-ios) (same author).
