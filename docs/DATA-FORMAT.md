# On-disk data format — byte-level contract

Every store here is deliberately byte-compatible with the Speechnotes iOS and
Linux-Swift apps so a library moves in either direction. `sn-core::appledate`
owns the one conversion that makes this work.

## Date encoding — the thing that breaks libraries

Swift's `JSONEncoder` defaults to `.deferredToDate`: **a JSON number of seconds
since 2001-01-01T00:00:00Z, always written as a float.**

```
encode(unix) -> unix - 2_082_844_800
decode(json) -> json + 2_082_844_800
```

`0` means 2001-01-01, not 1970-01-01. Getting this wrong shifts every note by
54 years and nothing errors.

## Null policy

Swift's synthesised `encode(to:)` writes `"deletedAt": null` for `nil`, and its
synthesised decoder **requires the key to exist**. So:

- **Writing:** emit every key, including explicit `null`s. Never
  `skip_serializing_if`.
- **Reading:** be lenient — any optional key may be absent (iOS writes some
  manifests without every optional), and any missing optional takes its default.

## Roots

```
$XDG_DATA_HOME/speechnotes/         (default ~/.local/share/speechnotes)
  notes.json          top-level array of Note
  notes.backup.json   rewritten every 10th save
  notebooks.json      top-level array of Notebook
  notebooks.backup.json
  bookmarks.json      object keyed "note:<uuid>" / "book:<uuid>:<chapter>"
  speechnotes.log
  Books/<uuid>/manifest.json, original.<ext>, cover.jpg, text/NNNN.txt
  models/{piper,kokoro,supertonic}/...
  Exports/*.wav
  note-images/<note-uuid>/<hash>.<ext>

$XDG_CONFIG_HOME/speechnotes/       (default ~/.config/speechnotes)
  prefs.json
```

Env overrides are read on **every** access and only honoured when absolute.

## `notes.json`

```json
[
  {
    "id": "3F2A1B4C-5D6E-4F70-8A91-B2C3D4E5F607",
    "explicitTitle": null,
    "text": "Body text.",
    "createdAt": 780000000.0,
    "updatedAt": 780000100.5,
    "deletedAt": null,
    "notebookId": null,
    "isPinned": false,
    "isFavorite": false
  }
]
```

| Key | Type | Required on read | Default |
|---|---|---|---|
| `id` | UUID string (uppercase hyphenated) | no | fresh UUID |
| `explicitTitle` | string \| null | no | `null` |
| `text` | string | no | `""` |
| `createdAt` | number (ref-date secs) | no | now |
| `updatedAt` | number | no | now |
| `deletedAt` | number \| null | no | `null` — non-null means binned |
| `notebookId` | UUID string \| null | no | `null` — null means Unfiled |
| `isPinned` | bool | no | `false` |
| `isFavorite` | bool | no | `false` |

Derived title: trimmed `explicitTitle` (first 120 chars), else the first
sentence of the first 300 characters (first 60 chars), else `"Untitled note"`.

Corrupt file → `notes.json.corrupt-<unix>` then try `notes.backup.json`.

## `notebooks.json`

```json
[{ "id": "<uuid>", "name": "Work", "createdAt": 780000000.0 }]
```

## `prefs.json`

Flat object, all keys always written.

| Key | Type | Default |
|---|---|---|
| `hasOnboarded` | bool | `false` |
| `activeNotebookScope` | string | `"all"` \| `"unfiled"` \| notebook UUID |
| `notesSortOrder` | string | `"edited"` \| `"created"` \| `"title"` |
| `renderMarkdown` | bool | `false` |
| `readAlongEnabled` | bool | `true` |
| `engineKind` | string | `"espeak"` \| `"piper"` \| `"pico"` \| `"kokoro"` \| `"supertonic"` |
| `voice` | string | `""` (legacy espeak-only) |
| `voicesByEngine` | object | `{}` |
| `rateMultiplier` | number | `1.0` |
| `appearance` | string | `"system"` \| `"light"` \| `"dark"` |
| `accentChoice` | string | one of 12 accents, `"blue"` |
| `readerTextScale` | number | `1.0` |

New keys this app adds (unknown to the others, ignored by them on read):

| Key | Type | Default |
|---|---|---|
| `audioRate` | number | `1.0` |
| `sleepTimerMinutes` | number | `0` (0 = off) |
| `editorMode` | string | `"edit"` \| `"preview"` \| `"read"` |
| `readerTheme` | string | `"light"` \| `"sepia"` \| `"dark"` \| `"trueBlack"` |
| `bookDropEnabled` | bool | `false` |

## `bookmarks.json`

```json
{
  "note:3F2A-...": {
    "noteId": "3F2A-...",
    "bookId": null,
    "chapterIndex": null,
    "textOffset": 412,
    "savedAt": 780000200.0
  }
}
```

`textOffset` is a **UTF-16 offset** for notes (iOS/Swift compatibility) and
**milliseconds** for book chapters. LRU-capped at 100.

## `Books/<uuid>/manifest.json`

```json
{
  "id": "...",
  "title": "...",
  "author": null,
  "format": "epub" | "pdf" | "audio"
          | "mobi" | "azw" | "azw3" | "prc" | "pdb"
          | "fb2" | "rtf" | "html" | "htm" | "htmlz"
          | "txt" | "text"
          | "pptx" | "ppsx" | "potx" | "ppt" | "odp"
          | "m4b" | "opus",
  "originalFileName": "book.epub",
  "addedAt": 780000000.0,
  "lastOpenedAt": null,
  "spineCount": null,
  "spine": null,
  "pageCount": null,
  "pdfChapters": null,
  "pdfChapterSource": null,
  "audioChapters": null,
  "audioChapterSource": null,
  "audioDuration": null,
  "hasCover": false,
  "toc": null,
  "position": null,
  "importError": null,
  "deletedAt": null
}
```

**Required on read:** `id`, `title`, `author`, `format`, `originalFileName`,
`addedAt`, `lastOpenedAt`, `hasCover`, `position`, `importError`, `deletedAt`.
Everything else is optional. **On write: emit every key**, nulls included —
Swift's strict decoder needs them.

`position` is `{"chapterIndex": n, "chapterFraction": f, "cfi": null}`.
`toc` entries are `{"label": ..., "href": ..., "spineIndex": ..., "depth": ...}`.
`pdfChapterSource`: `"outline"` | `"headings"` | `"pages"`.
`audioChapterSource`: `"ffprobe"` | `"chpl"` | `"id3"` | `"track"` | `"single"`.

**This app's improvement over the Swift version:** `audioChapters` is `null`
when a file carries no chapter markers. The Swift app wrote a fake
`"Full audiobook"` chapter and then needed a UI special case to hide it.

### Headless document formats — the no-conversion guarantee

The Mobipocket/Kindle family (`mobi`/`azw`/`azw3`/`prc`/`pdb`), FictionBook
(`fb2`), RTF, HTML (`html`/`htm`/`htmlz`) and plain text (`txt`/`text`) are
supported **natively**: each is parsed by its own clean-room reader in
`sn-doc`, and the original file is stored **verbatim** as
`original.<real-ext>` (`original.mobi`, `original.fb2`, …). The manifest's
`format` records the real lowercase extension — never `"epub"` — and
**no `.epub` is ever written**; the iOS app's convert-to-EPUB step is
deliberately not ported. Chapters are synthesised in memory
(`chapter-0`, …, the spine) and served through the reader's web engine, and
the speech path extracts them with the ordinary chapter pass, so bookmarks,
search, read-along and resume work unchanged. A document's `cover.jpg` is
extracted from the parse when the format carries one; the `.pdb` extension
is gated with a content sniff because it is shared with non-book Palm-era
formats, and a DRM-locked book is refused with a typed message rather than
half-decoded.

## BookDrop

The receiver's accept gate (`sn_net::bookdrop::ACCEPTED_EXTENSIONS`, applied
at prepare-upload) admits: `epub pdf mobi azw azw3 prc pdb fb2 rtf html htm
htmlz` → the book library, `m4b m4a mp4 mp3 opus ogg oga` → the audiobook
library, `jex` → notes + notebooks, `md markdown txt text` → one note each
(a `.txt` over BookDrop stays a note; the file chooser is where a `.txt`
becomes a plain-text book), `docx` → one note with its images. Anything
else — `wav` among it, which lands in iOS's exports folder, a folder this
app does not have — counts as rejected: the spec's partial accept.

## Images

`note-images/<note-uuid>/<djb2-hex>.<ext>`; the markdown target is
`speechnotes://note-image/<name>`. Extension from magic bytes first, else the
supplied one, lowercase, alnum only, ≤ 8 chars.

## Logs

`dataDir/speechnotes.log`, lines of
`[<ISO8601>] <LEVEL>: <message>`, 2 MiB cap keeping the last 1 MiB, plus a
300-line in-memory ring the UI reads.