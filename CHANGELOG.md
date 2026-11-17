# Changelog

All notable changes to Speechnotes Linux are documented here. The format is
based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the
project adheres to [Semantic Versioning](https://semver.org/).

## [0.1.0-alpha.1] — 2026-11-18

First public alpha. Everything below ships in this release.

### Added

- Notes: markdown editor, notebooks, bookmarks, search, note images,
  `.docx`/`.jex` import/export
- Speech: read-along TTS for notes and books, with five engines —
  espeak-ng (built in), Kokoro-82M, nano-edge-tts, Supertonic, Phonon —
  and a Models page that downloads engines on demand with sha256-pinned,
  floor-checked transfers
- Books: EPUB, PDF, MOBI/AZW3/PDB, FB2, RTF, HTML/HTMLZ readers; slide
  decks (PPTX/PPT/ODP) with a presenter window; audiobook import
  (`.m4b`/`.opus`/`.mp3`) and `.wav` export
- OCR for scanned PDF pages (RapidOCR; tesseract backend option)
- BookDrop: LocalSend-compatible LAN transfer (ports 53317–53327)
- Pomodoro timer with structured plans; four appearance schemes including
  E-Ink; reading typography controls
- Headless CLI: `--speak`, `--engines`, `--receive`, `--import-book`,
  `--import-note`, `--import-audiobook`, `--play-audiobook`, `--read-book`,
  `--open-book`, `--play-note`, `--books`, `--selftest`

### Notes

- Data is plain JSON under `$XDG_DATA_HOME/speechnotes` — see
  [docs/DATA-FORMAT.md](docs/DATA-FORMAT.md)
- Neural model files download from HuggingFace at first use
