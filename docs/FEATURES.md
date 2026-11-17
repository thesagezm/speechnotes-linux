# Features

## Notes

- Markdown-flavored note editor with live preview, math (KaTeX-style inline
  and display), fenced code, tables and task lists
- Notebooks, pins, bookmarks, tags, full-text search with a query language
  (scopes, filters, sort orders)
- Import/export of `.docx` and Joplin `.jex` interchange files
- Note images with an `speechnotes://note-image/` scheme, deduplicated by hash
- Slash commands, keyboard-first shortcuts, an outline/TOC sidebar

## Speech

- Read any note or book aloud with follow-along highlighting; the voice
  follows the page in the reader and restarts where the page shows
- Five engines, hot-switchable:
  - **espeak-ng** — built in, instant, every language espeak ships
  - **Kokoro-82M** — 82M-param neural TTS, 24 kHz, downloaded from
    HuggingFace on demand
  - **nano-edge-tts** — a 1.75M-param distilled decoder, ~33× faster than
    Kokoro, same voice banks
  - **Supertonic** — Supertone's expressive engine
  - **Phonon** — gradium's 31M English engine
- Speak from selection; per-note voice, speed and pitch memory; engines are
  managed on a Models page (download, size floors, sha256 pins)
- Audiobooks: generate `.wav` exports per note/book, import `.m4b`/`.opus`/
  `.mp3` audiobooks into the library, play with chapter/seek controls

## Book reader

- Formats: **EPUB**, **PDF**, **MOBI/AZW3/PDB**, **FB2**, **RTF**,
  **HTML/HTMLZ**, slide decks (**PPTX/PPT/ODP**)
- EPUB renders as the publisher made it (real web engine, publisher
  stylesheets); paginated and scroll modes
- PDF with text extraction, page thumbnails, per-page text selection, and
  OCR for scanned pages (RapidOCR, with tesseract as the backend option)
- Speaker decks render as slides with a full-screen presenter window
- TOC, bookmarks, per-book resume, read-along TTS, chapter navigation

## Focus & productivity

- Pomodoro timer: classic rounds and structured plans ("3 × 25 min, 5 min
  break", "finish by 17:00"), chimes, session notes
- Tasks and checklists inside notes; today's plan summary
- E-Ink-friendly theme, four appearance schemes (System/Light/Dark/Warm +
  E-Ink), reading typography controls (family, size, line height, margins)

## Getting content in and out

- **BookDrop**: send books and notes from a phone over the LAN —
  LocalSend-compatible receiver/sender on ports 53317–53327
- Drag-and-drop import; `speechnotes --import-book …` from the CLI
- Exports: plain text, audio (`.wav`), JEX interchange
- Data is plain JSON on disk (see [DATA-FORMAT.md](DATA-FORMAT.md)) — no
  lock-in, byte-compatible with the iOS app so a library moves both ways

## Platform

- GTK4 + libadwaita-era idioms, Wayland-native (app-id matched launcher
  icon), keyboard accessible (ATK/a11y tree coverage)
- Offline-first: every engine runs locally; nothing leaves the machine
  except the model downloads you choose to make
- Headless CLI for scripts and servers (`--speak`, `--receive`, `--import-*`)
