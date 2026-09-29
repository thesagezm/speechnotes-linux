# Speechnotes Linux — iOS feature-gap analysis & implementation plan

Compared 2026-09-29, speechnotes-ios `main` (v1.7.1 lineage) against
speechnotes-linux `main` (Phases 0–13 shipped).

Method: every file under `speechnotes-ios/App/Sources/Views/` and the
services it drives was checked for a Linux counterpart under
`speechnotes-linux/Sources/`. Desktop-inapplicable features (lock-screen
Now Playing, haptics, share sheets, orientation handling) are out of scope
by definition. Where a store/API already exists on Linux and only the UI is
missing, that is called out — those are cheap wins.

## What Linux already has (baseline)

NavigationSplitView shell (sidebar scopes / notes list / editor), five-engine
TTS tier (espeak, pico, piper, Kokoro, Supertonic) with the dsnote engine
contract, read-along bookmarks + auto-resume banner, Books (EPUB / PDF /
audiobooks) with chapter nav, chpl parser at iOS post-fix semantics, JEX
import/export, WAV render, recycle bin (notes), byte-compatible data layer.

## Gaps, prioritized

### P1 — the ones users hit daily

| # | Feature (iOS source) | Linux state | Notes |
|---|----------------------|-------------|-------|
| 1 | **Appearance settings** (`AppearanceSettingsView`, `AppTheme`): accent color (12 choices), theme System/Light/Dark, reader text scale, reader line/block/table spacing | **None.** No theme control at all; app renders light regardless of the desktop scheme | **Shipped 2026-09-29 (this pass)**: Theme system/light/dark (system via GSettings color-scheme + GtkSettings fallbacks), accent color applied through GTK named colors + SCU tints, editor/reader text size. Reader spacing sliders still open. |
| 2 | **Global mini-player** (`MiniPlayerBar`, `GlobalMiniPlayerOverlay`, `BookPlayerBar`): playback survives navigation; one persistent transport for note TTS, book TTS, audiobooks and exported-WAV playback | Transport lives inside the editor/reader panes only — navigate away and you lose all controls | Add a bottom transport bar in `AppShell`, driven by `TTSController`/`AudioBookController` state; show title, %, pause/resume/stop. All controller state it needs already exists. |
| 3 | **Voice picker** (`VoicePickerSheet`, `VoiceCatalog`): 28 Kokoro voices with friendly names + descriptions, searchable, sectioned, recents, tap-to-hear-sample; Supertonic voice styles + language | `prefs.voice` exists and the engines honor it, but there is **no UI** — users are stuck on the engine default voice | Port `VoiceCatalog` (pure data) to `Data` or `SpeechLogic`; new Voice pane listing voices for the active engine with a sample button (synthesize a fixed sentence to a temp WAV and play through `TTSPlayer`/`BookAudioPlayer`); recents in prefs. |
| 4 | **Storage manager** (`StorageSettingsView`, `ExportsStore`): disk usage breakdown, **exports browser** (list exported WAVs, play them, delete), image-cache clearing, model deletion | WAV renders land in `Exports/` with no way to browse, play or delete them from the app; models can be downloaded but not deleted | New Storage pane: exports list (size + date via FileManager), play via `BookAudioPlayer` (ffmpeg handles WAV), delete; "delete model" buttons per engine; usage summary. |
| 5 | **Engine model state UX** (`SettingsView`): download progress, ready/failed states, retry, model tiers (Kokoro fp32 vs uint8) | Three flat download buttons sharing one status string; no progress, no ready-state, no delete | Rework into per-model rows with state + ProgressView; expose Kokoro tier pref (`KOKORO_TIER`). |

### P2 — important, small-to-medium

| # | Feature (iOS source) | Linux state | Notes |
|---|----------------------|-------------|-------|
| 6 | **Books bin UI** (`BooksRecycleBinView`) | `BooksStore.restore/purge/emptyRecycleBin/deletedBooks` all exist; **no UI** | Add a "Binned books" section to BooksPane (or a books tab in RecycleBinPane). Pure UI work. |
| 7 | **Markdown preview mode** (`MarkdownPreviewView`, `renderMarkdown`) | `renderMarkdown` pref + `MarkdownText` logic ported, but the editor never renders it | Preview toggle in the editor: swap `TextEditor` for a styled read-only `ScrollView`/`Text` view rendering through `MarkdownText`. Headings/emphasis/lists only (no images, see #11). |
| 8 | **Full read-along view** (`ReadAlongView`) | Read-along is a one-line strip under the editor toolbar | A reader-style view: current sentence emphasized (accent), neighboring sentences dimmed, text at reader scale. Controller already publishes `currentSentence` + position. |
| 9 | **Backup scope** (`BackupExportView`): JEX export of all notes or a selected subset of notebooks | Linux JEX export is all-or-nothing | Add a notebook multi-select before export (JexPayloads already filters; just feed it fewer notes). |
| 10 | **About + Logs panes** (`AboutView`, `LogsView`, `LogStore`) | Log goes to a file; nothing in-app | Trivial panes: version/commit/licenses; tail of `AppPaths.logFile` in a ScrollView with refresh. |
| 11 | **Images in notes** (`ImagePicker`, `ZoomableImageView`, `MarkdownImageInserter`, `RemoteImageStore`) | `NoteImageStore` ported; no insertion UI, no rendering | **Editing-side blocked** by SwiftCrossUI: `TextEditor` is a bare `Binding<String>` (no attachments). Possible once preview mode (#7) exists: render `![](image:…)` in preview via `Image`. Defer. |
| 12 | **Notebook management UI** (`NotebookListView`): rename/delete notebooks | Create only | Add a ⋯ menu per notebook row (rename via TextField dialog, delete with confirm via `presentAlert` env action). |
| 13 | **Onboarding** (`OnboardingView`) | `hasOnboarded` pref exists, never used | Simple first-launch welcome pane (what the app does, engine download pointers). |

### Deferred / blocked by SwiftCrossUI 0.9.0

| Feature | Blocker |
|---------|---------|
| Slash-menu overlay (`MarkdownSlashMenu` UI) | Logic is already ported, but `TextEditor` exposes **no caret/selection API**, so trigger detection can't run. Revisit on SCU upgrade or with a custom GtkTextView representable. |
| Formatting bar (`MarkdownFormattingBar`) | Same caret limitation — without a cursor, "insert at position" degrades to append-at-end. |
| EPUB web view (`BookWebView`) | iOS renders EPUB in WKWebView with styling; Linux shows extracted plain text. A styled reader could be built from XhtmlText + rich Text, but it is cosmetic, not functional. |
| Waveform/scrub bar (`PlaybackRail`) | `ScrollView` has no programmatic scroll-to and no gesture APIs for a scrubber. Seek exists in the controllers, so any slider can drive it — a plain Slider-based seek is feasible in #2's transport bar. |
| Haptics, Now Playing Center, share sheets, orientation | Not applicable on desktop. |

## Suggested implementation order

1. **This pass (2026-09-29)** — Appearance infra + UI polish: Prefs keys
   (`appearance`, `accentChoice`, `readerTextScale`), `Appearance` module
   (ThemeController: GSettings/GtkSettings system-scheme detection, GTK
   named-color + window/textview CSS, accent palette), Settings redesign
   (sections, switches, menu pickers, appearance section), sidebar/editor/
   reader polish (accent tints, typography, hover states via CSS).
2. **Next** — #2 global mini-player (biggest UX gap for a TTS app), #6
   books bin UI, #10 About/Logs panes (all cheap).
3. **Then** — #3 voice picker (port VoiceCatalog first: pure data +
   tests), #4/#5 storage & model management.
4. **Later** — #7 preview mode, #8 read-along view, #9 backup scope, #12
   notebook menus, #13 onboarding.
5. **Blocked pending SCU** — #11 images (needs preview mode first), slash
   menu + formatting bar (needs TextEditor caret access).

Verification notes for whoever picks this up: every feature above that
touches TTS keeps the soft-fail philosophy (one attempt, status line, never
a crash); data-layer work reuses the byte-compatible stores and their test
suites; CI has no ffmpeg/poppler/pico — those tests are skip-guarded, keep
it that way.
