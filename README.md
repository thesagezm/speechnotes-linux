# Speechnotes-Linux

A native Linux desktop notes app that reads your notes aloud, offline. Swift +
GTK4 (via SwiftCrossUI), with the multi-engine TTS architecture used by
[Speech Note (dsnote)](https://github.com/mkiol/dsnote) — an abstract engine
contract, pluggable backends, and a model manager that downloads voices on
demand.

Personal project. **GPL-3.0** (espeak-ng and piper are GPL-3.0; SwiftCrossUI
and ONNX Runtime are MIT and fine as libraries).

## Status

**Phase 0 complete (stack de-risked).** The three gates that could have sunk
the Swift/GTK plan all pass on this machine (Pop!_OS 24.04, COSMIC/Wayland):

| Gate | Result |
|---|---|
| GTK4 window from Swift | Verified rendered on the compositor (native widgets, text, button borders) via both SwiftCrossUI and a raw C-API window |
| ALSA playback from Swift | Verified: 44.1 kHz tone through the default device, write + delay + drain all clean |
| espeak-ng speech from Swift | Verified: `AUDIO_OUTPUT_RETRIEVAL` callback → Int16 PCM → ALSA, multiple voices, 22.05 kHz |

Phase 1 (the `SpeechLogic` port) is the next step.

## Engines

The engine list mirrors the iOS app's tiering, and dsnote's shape:

| Engine | Model | Size | Quality |
|---|---|---|---|
| eSpeak-NG | none | 0 | robotic, instant, 131 voices |
| Pico2Wave | none | 0 | light, 4 languages |
| Piper (VITS) | `.onnx` + `.onnx.json` | 20–130 MB | natural neural |
| Kokoro-82M | ONNX (uint8/fp32) | 86–326 MB | excellent, 28 voices |
| Supertonic | 4-stage ONNX | ~400 MB | excellent, 31 languages |

Each engine implements four hooks — `model_created()`,
`model_supports_speed()`, `create_model()`, `encode_speech_impl(text, speed,
out_file)` — and the base class owns the worker thread, the task queue, the
state machine, and sentence splitting. Speech-to-text is deliberately **not**
part of this app: TTS is the mission.

## Build & run

```sh
Scripts/bootstrap.sh                      # one-time: toolchain, symlink, build, gates
GDK_BACKEND=wayland ./.build/debug/speechnotes-linux
```

Phase-0 diagnostics:

```sh
./.build/debug/gtk-smoke                  # a raw GTK4 window (self-closing after 6s)
./.build/debug/alsa-tone                  # a 440 Hz tone
./.build/debug/espeak-say en-us "hi"      # one spoken sentence
./.build/debug/espeak-say --list          # the voice identifiers espeak exposes
```

## Machine notes (why the code looks the way it does)

- **No root.** Everything installs under `$HOME`. The one system gap:
  `libespeak-ng.so` (the unversioned linker name) doesn't exist without the
  `-dev` package, so `bootstrap.sh` symlinks it in `~/.local/lib`.
- **Swift via swiftly.** SwiftUI's idioms survive: SwiftCrossUI's
  `@State`/`@Environment`/`ObservableObject` map cleanly onto the iOS app's
  view code, which is what makes the three-app family (iOS/Android/Linux)
  maintainable by one person.
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
                   XhtmlText, WAVWriter, JexExport, …)
  EspeakBridge/    Swift bridge over libespeak-ng (retrieval mode)
  AlsaSink/        ALSA playback + position clock
  AppPaths/        XDG data/cache/config roots
  Log/             ring buffer + on-disk tail
  SpeechnotesLinux/ the GTK app
  GTKSmoke/        Phase-0 diagnostics
  AlsaTone/        Phase-0 diagnostics
  EspeakSay/       Phase-0 diagnostics
ThirdParty/
  espeak-ng-headers/  vendored 1.51 headers (speak_lib.h, espeak_ng.h)
Tests/SpeechLogicTests/  (coming with Phase 1)
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
- [Supertonic](https://github.com/supertone-oss-archive/supertonic) —
  code MIT, model OpenRAIL-M.
- Piper voices — `rhasspy/piper-voices`, per-voice MODEL_CARD licenses.
- Ported code from [speechnotes-ios](../speechnotes-ios) (same author).
