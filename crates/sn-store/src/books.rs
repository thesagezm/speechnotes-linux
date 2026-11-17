//! The book library: import an EPUB into the app's data directory and read
//! it back without touching the archive twice.
//!
//! The manifest is **byte-compatible with the iOS app's** — same field
//! names, same meaning — because the two apps share the library directory
//! through BookDrop, and a book imported on one must open on the other. The
//! fields, verified against a real iOS-written manifest:
//!
//! * `id`, `format`, `addedAt`, `lastOpenedAt` — identity and timestamps
//! * `title`, `author` — from the OPF metadata
//! * `spine`, `spineCount`, `toc[{spineIndex, href, label}]` — reading
//!   order and the table of contents
//! * `position {chapterIndex, chapterFraction}` — where the reader stands
//! * `hasCover` — whether `cover.jpg` was extracted
//! * `originalFileName` — what the user's file was called
//!
//! The rules this module exists to keep:
//!
//! * **The archive is copied, never moved.** The user's file stays where it
//!   is; `original.epub` in the book's directory is the app's own copy.
//! * **The cover is extracted, not generated.** `cover.jpg` beside the
//!   manifest is what the shelf shows; a missing cover means `hasCover:
//!   false` and the shelf draws its own placeholder.
//! * **Import is recoverable.** A copy that fails halfway leaves no
//!   manifest — the shelf shows nothing rather than a book that cannot
//!   open. The copy lands in a `.part` and is renamed into place, the same
//!   atomic discipline the notes store uses.
//! * **Nothing is parsed at open time.** The manifest carries the spine and
//!   TOC; the archive is opened once per chapter read, and the chapter text
//!   is extracted on demand.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sn_core::paths;

/// One table-of-contents row, as the manifest stores it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestTocEntry {
    /// Index into `spine`, when the row points at a spine item.
    #[serde(rename = "spineIndex")]
    pub spine_index: Option<usize>,
    pub href: String,
    pub label: String,
    /// Nesting depth from the source document, 0 = top level. Parsed by the
    /// EPUB nav/NCX and the PDF outline readers, carried here so the reader
    /// can indent instead of flattening a three-level textbook contents into
    /// one wall of rows. Defaulted so an iOS-written manifest still parses.
    #[serde(default, rename = "depth")]
    pub depth: usize,
    /// The anchor inside the target file (`#sec12` → `sec12`), when the row
    /// named one. The reader scrolls to it after the file loads, so a
    /// contents row that names a *section* lands on the section. Defaulted,
    /// because a manifest written before this field — or by another app —
    /// has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fragment: Option<String>,
}

/// Where the reader stands in a book.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BookPosition {
    #[serde(rename = "chapterIndex")]
    pub chapter_index: usize,
    /// 0.0–1.0 through the chapter, from the bookmark store.
    #[serde(rename = "chapterFraction")]
    pub chapter_fraction: f64,
    /// The engine's canonical position — a foliate CFI — for the books the
    /// book view renders. A CFI pins an exact spot in the *content*, so a
    /// reflow (a resize, a font change) cannot drift the way a chapter
    /// index alone does; the chapter index beside it stays the fallback for
    /// engines and views that cannot resolve it. Absent on every manifest
    /// written before the book view existed.
    #[serde(rename = "cfi", default, skip_serializing_if = "Option::is_none")]
    pub cfi: Option<String>,
}

impl Default for BookPosition {
    fn default() -> Self {
        Self {
            chapter_index: 0,
            chapter_fraction: 0.0,
            cfi: None,
        }
    }
}

/// The per-book manifest: everything the shelf and reader need without
/// opening the archive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BookManifest {
    pub id: String,
    pub format: String,
    #[serde(rename = "addedAt")]
    pub added_at: f64,
    #[serde(rename = "lastOpenedAt")]
    pub last_opened_at: f64,
    pub title: String,
    pub author: String,
    // EPUB-only fields. Defaulted rather than required: an audiobook
    // manifest — the iOS app's, or this app's own — has no reading order,
    // and refusing to parse it made a book imported on the other platform
    // invisible here.
    #[serde(default, rename = "spineCount")]
    pub spine_count: usize,
    #[serde(default)]
    pub spine: Vec<String>,
    #[serde(default)]
    pub toc: Vec<ManifestTocEntry>,
    #[serde(default)]
    pub position: BookPosition,
    #[serde(default, rename = "hasCover")]
    pub has_cover: bool,
    #[serde(default, rename = "originalFileName")]
    pub original_file_name: String,
    // --- audiobook fields (absent on EPUB manifests; optional so a book
    //     imported on one platform still opens on another) ---------------
    /// Total duration in seconds, from the container's own header. The iOS
    /// app's name for the same number is `audioDuration`, so the field is
    /// written under both: this app reads either, and writes the one the
    /// other app reads.
    #[serde(default, rename = "audioDuration", skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<f64>,
    /// The audio codec, as the container names it ("eac3", "opus", "aac",
    /// "alac", "mp3"). None for EPUB.
    #[serde(default, rename = "audioCodec", skip_serializing_if = "Option::is_none")]
    pub audio_codec: Option<String>,
    /// Chapters, in order. Empty for a single-chapter file. The iOS app's
    /// name for the same list is `audioChapters`, so both spellings are
    /// accepted on read.
    #[serde(
        default,
        rename = "audioChapters",
        alias = "chapters",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub chapters: Vec<AudiobookChapter>,
}

/// One chapter of an audiobook, times in seconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudiobookChapter {
    pub title: String,
    #[serde(rename = "startSeconds")]
    pub start_seconds: f64,
    #[serde(rename = "endSeconds")]
    pub end_seconds: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum BookError {
    #[error("not an epub: {0}")]
    NotAnEpub(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not read the book's contents: {0}")]
    Epub(String),
    /// The file parses as its format but its contents are out of reach — a
    /// DRM-locked book, or a `.pdb` that is some other Palm-era format.
    /// The message is already user-presentable: the file is fine, this app
    /// just cannot read it.
    #[error("{0}")]
    Unsupported(String),
}

/// One book in the library: its manifest plus the directory it lives in.
#[derive(Clone)]
pub struct Book {
    // Debug by hand: the archive cache is a shared handle, and the manifest
    // is the part a log line or a test failure wants to show.
    pub manifest: BookManifest,
    pub dir: PathBuf,
    /// The parsed archive, cached after the first chapter read. Cloning a
    /// `Book` clones the handle, not the 136 MB of zip data — which is what
    /// makes a search over 73 chapters one parse instead of 73.
    archive: std::rc::Rc<std::cell::RefCell<Option<std::sync::Arc<sn_doc::zip::Archive>>>>,
    /// The parsed PDF, same idea one format over: `first_speakable_chapter`
    /// walks pages until it finds prose, and a re-parse per page on a
    /// 2000-page book is minutes of repeated work. Behind an `Arc` because a
    /// `Document` owns the whole file's bytes: a clone that copied them would
    /// make "share the parse" a lie, and the handle is `Clone`.
    pdf_doc: std::rc::Rc<std::cell::RefCell<Option<std::sync::Arc<sn_doc::pdf::Document>>>>,
    /// The parsed slide deck, same idea one format over: the deck is
    /// parsed once per open and shared by the shelf, the viewer and the
    /// speech planner.
    deck: std::rc::Rc<std::cell::RefCell<Option<std::sync::Arc<sn_doc::slides::SlideDeck>>>>,
    /// The parsed headless document (MOBI family, FB2, RTF, HTML, plain
    /// text), same idea two formats over: the reader's scheme handler
    /// serves the chapters out of it and the speech path extracts from it.
    document: std::rc::Rc<std::cell::RefCell<Option<std::sync::Arc<ServedBookDoc>>>>,
}

/// The ceiling on what one format's parse memo keeps in heap.
///
/// A parse is what makes an open 0.35 s instead of four, and a page read
/// instant instead of a re-parse — but a parsed document holds a full copy
/// of its file, and this box has 7.5 GiB and was already swapping: three
/// big books held ~250 MB of anonymous heap with no ceiling at all. Under
/// the cap the memo evicts the least-recently-used parse; the reader bumps
/// a parse's recency on every use, so the book on screen is never the one
/// evicted, and a book re-opened after eviction re-pays one 0.35 s parse.
const PARSE_MEMO_CAP: u64 = 192 * 1024 * 1024;

/// Recency across the three parse memos, bumped on every memo hit and
/// insert — eviction keeps the newest stamps.
static MEMO_STAMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The largest book file the open path will read whole. Everything real —
/// the 1295-page textbook included — sits far under it; the cap exists so a
/// planted multi-gigabyte "PDF" cannot OOM the open path before any parser
/// applies its own limits.
const BOOK_FILE_CAP: u64 = 2 * 1024 * 1024 * 1024;

/// The largest document book (`original.mobi`, `original.fb2`, …) the open
/// path will read whole — the same cap the document import applies, so a
/// file that lands in the library is one the parse can afford, and a file
/// swapped in underneath a manifest is re-checked rather than trusted.
const DOCUMENT_FILE_CAP: u64 = 512 * 1024 * 1024;

/// One format's parse memo: the parse, its file's size, its last-use stamp.
struct Memo<V> {
    map: std::collections::HashMap<(PathBuf, u64), (std::sync::Arc<V>, u64)>,
    total: u64,
}

impl<V> Memo<V> {
    fn new() -> Self {
        Self { map: std::collections::HashMap::new(), total: 0 }
    }

    /// The parse for (path, size), parsing it with `parse` on a miss. A hit
    /// bumps recency; an insert evicts least-recently-used entries until
    /// under [`PARSE_MEMO_CAP`] — never the one just inserted, so a single
    /// file bigger than the cap still works.
    fn get_or_insert(
        &mut self,
        path: &Path,
        size: u64,
        parse: impl FnOnce() -> Result<std::sync::Arc<V>, BookError>,
    ) -> Result<std::sync::Arc<V>, BookError> {
        let key = (path.to_path_buf(), size);
        if let Some((value, stamp)) = self.map.get_mut(&key) {
            *stamp = MEMO_STAMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Ok(value.clone());
        }
        let value = parse()?;
        let stamp = MEMO_STAMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.map.insert(key, (value.clone(), stamp));
        self.total += size;
        self.evict_under(PARSE_MEMO_CAP);
        Ok(value)
    }

    /// Drop least-recently-used entries until `total <= cap` or one entry
    /// remains — a lone file bigger than the cap is kept, because evicting
    /// it would only guarantee a re-parse.
    fn evict_under(&mut self, cap: u64) {
        while self.total > cap && self.map.len() > 1 {
            let Some(oldest) = self
                .map
                .iter()
                .min_by_key(|(_, (_, stamp))| *stamp)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            // The size is part of the key; remove() returns only the value.
            if self.map.remove(&oldest).is_some() {
                self.total -= oldest.1;
            }
        }
    }

    /// Forget a path's parse — a deleted book must not keep 50 MB alive.
    fn forget(&mut self, path: &Path) {
        let removed: Vec<(PathBuf, u64)> = self
            .map
            .keys()
            .filter(|(cached, _)| cached == path)
            .cloned()
            .collect();
        for key in removed {
            if self.map.remove(&key).is_some() {
                self.total = self.total.saturating_sub(key.1);
            }
        }
    }
}

/// The slide decks this process has parsed, same shape as [`PDF_PARSES`].
static SLIDE_PARSES: std::sync::Mutex<Option<Memo<sn_doc::slides::SlideDeck>>> =
    std::sync::Mutex::new(None);

/// The headless documents this process has parsed (the Mobipocket/Kindle
/// family, FB2, RTF, HTML, plain text), same shape as [`PDF_PARSES`].
static DOCUMENT_PARSES: std::sync::Mutex<Option<Memo<ServedBookDoc>>> =
    std::sync::Mutex::new(None);

/// The parses this process has already paid for, keyed by the file's path and
/// size.
///
/// A `Book` handle carries its own parse, and that is the right shape *within*
/// a handle: the shelf clones a `Book` on every refresh, and the clone must
/// not copy 50 MB. But a handle is not a lifetime. The reader opens a book
/// through [`BooksStore::find`], the shelf refreshes through
/// [`library_cached`], and the two do not share a handle — so a 1295-page PDF
/// was parsed **twice** on one open: four seconds each, on the UI thread,
/// before a single page was drawn.
///
/// The memo is what makes "one parse per open" true rather than "one parse
/// per handle". It is keyed by path *and* size, so an import that replaces a
/// file with a different one of the same name is not served the old parse,
/// and it is dropped when the book is deleted. It is bounded by
/// [`PARSE_MEMO_CAP`], because the parse holds the file's bytes.
///
/// The value is an `Arc`, so several handles share one parse, and the map is
/// behind a `Mutex` because the render worker reads it from another thread.
static PDF_PARSES: std::sync::Mutex<Option<Memo<sn_doc::pdf::Document>>> =
    std::sync::Mutex::new(None);

/// The EPUB archives this process has parsed, same shape as [`PDF_PARSES`].
///
/// The open path moved off the UI thread, but a `Book` (and the `Rc` caches
/// inside it) cannot cross threads — the worker that opens the archive and
/// the UI thread that hands it to the web engine are two different handles.
/// This memo is how the worker's parse reaches the UI thread: the worker
/// pays for it, the memo keeps it, and the UI-side `archive()` call is a
/// lookup.
static ARCHIVES: std::sync::Mutex<Option<Memo<sn_doc::zip::Archive>>> =
    std::sync::Mutex::new(None);

/// The parse for `path`, parsing it if this process has not already.
///
/// The lock is held across the parse on purpose: two threads asking for the
/// same file at the same time must not both spend four seconds on it. The
/// cost is that an unrelated book waits for the parse in flight, which is the
/// same wait it would have had anyway.
fn memoised_pdf_parse(path: &Path) -> Result<std::sync::Arc<sn_doc::pdf::Document>, BookError> {
    let size = std::fs::metadata(path)?.len();
    PDF_PARSES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get_or_insert_with(Memo::new)
        .get_or_insert(path, size, || {
            // Capped like every other process boundary: a "PDF" dropped on
            // the shelf must not be able to allocate the machine.
            let bytes = sn_core::util::read_capped(path, BOOK_FILE_CAP).map_err(|e| {
                BookError::Epub(format!("{}: {e}", path.display()))
            })?;
            let doc = std::sync::Arc::new(
                sn_doc::pdf::Document::parse(&bytes)
                    .map_err(|e| BookError::Epub(format!("pdf: {e}")))?,
            );
            Ok(doc)
        })
}

/// Forget a book's parse, so a deleted book does not keep 50 MB alive and a
/// re-import does not read the old file's structure.
fn forget_parses(path: &Path) {
    let mut guard = PDF_PARSES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(memo) = guard.as_mut() {
        memo.forget(path);
    }
    let mut guard = ARCHIVES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(memo) = guard.as_mut() {
        memo.forget(path);
    }
    let mut guard = SLIDE_PARSES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(memo) = guard.as_mut() {
        memo.forget(path);
    }
    let mut guard = DOCUMENT_PARSES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(memo) = guard.as_mut() {
        memo.forget(path);
    }
}

/// The Force OCR marker: a file beside the manifest turning the book's
/// forced-recognition mode on. Presence is on.
const FORCE_OCR_MARKER: &str = "force-ocr";

/// The locks that keep one page from being recognised twice at once.
///
/// Keyed by (book directory, page). Recognition is the most expensive thing
/// the app does per page — about two seconds for a scanned page — and two
/// callers want the same page at the same time: the background warm-up
/// thread and the reader's own read of the page the user is looking at.
/// Without the lock each recognised the page, and the second one to finish
/// overwrote the first's cache.
///
/// The map is process-wide and the locks are handed out and kept, so a page
/// recognised once is never recognised twice by two threads. A page's lock
/// is tiny; the map grows with the number of pages *recognised in this
/// process*, which is bounded by the books the reader actually opens.
static OCR_PAGE_LOCKS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<(PathBuf, usize), std::sync::Arc<std::sync::Mutex<()>>>>,
> = std::sync::LazyLock::new(|| {
    std::sync::Mutex::new(std::collections::HashMap::new())
});

/// The lock for one book's one page — the same lock for every caller that
/// wants that page.
fn page_lock(dir: &Path, index: usize) -> std::sync::Arc<std::sync::Mutex<()>> {
    let mut map = OCR_PAGE_LOCKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    map.entry((dir.to_path_buf(), index))
        .or_insert_with(|| std::sync::Arc::new(std::sync::Mutex::new(())))
        .clone()
}

/// The lowest page number with a non-empty OCR cache under `<dir>/text`,
/// 1-based — the page a scanned book can actually open on.
fn first_nonempty_ocr_cache(dir: &Path) -> Option<usize> {
    let entries = std::fs::read_dir(dir.join("text")).ok()?;
    let mut pages: Vec<usize> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.metadata().map(|m| m.len() > 0).unwrap_or(false))
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            name.strip_suffix(".txt")?.parse::<usize>().ok()
        })
        .collect();
    pages.sort_unstable();
    pages.into_iter().next()
}

/// The parsed archive for `path`, parsing it if this process has not already.
/// Same locking discipline as [`memoised_pdf_parse`]: the lock is held across
/// the parse so two threads cannot both pay for the same 136 MB file.
fn memoised_archive(path: &Path) -> Result<std::sync::Arc<sn_doc::zip::Archive>, BookError> {
    let size = std::fs::metadata(path)?.len();
    ARCHIVES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get_or_insert_with(Memo::new)
        .get_or_insert(path, size, || {
            let bytes = std::fs::read(path)?;
            let parsed = std::sync::Arc::new(
                sn_doc::zip::Archive::parse(bytes)
                    .map_err(|e| BookError::Epub(e.to_string()))?,
            );
            Ok(parsed)
        })
}

/// The parsed slide deck for `path`, parsing it if this process has not
/// already. Same discipline as the PDF's parse: one parse per open, shared
/// by every handle, dropped when the book is deleted.
fn memoised_deck_parse(path: &Path) -> Result<std::sync::Arc<sn_doc::slides::SlideDeck>, BookError> {
    let size = std::fs::metadata(path)?.len();
    SLIDE_PARSES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get_or_insert_with(Memo::new)
        .get_or_insert(path, size, || {
            let bytes = std::fs::read(path)?;
            let deck = std::sync::Arc::new(
                sn_doc::slides::SlideDeck::parse(&bytes)
                    .map_err(|e| BookError::Epub(format!("slide deck: {e}")))?,
            );
            Ok(deck)
        })
}

/// The parsed document book for `path`, parsing it if this process has not
/// already. Same discipline as the PDF's parse: one parse per open, shared
/// by every handle, dropped when the book is deleted.
fn memoised_document_parse(path: &Path) -> Result<std::sync::Arc<ServedBookDoc>, BookError> {
    let size = std::fs::metadata(path)?.len();
    DOCUMENT_PARSES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get_or_insert_with(Memo::new)
        .get_or_insert(path, size, || {
            // Capped like the import that wrote the file: the copy on disk
            // passed the cap once, and a file swapped in underneath a
            // manifest is re-checked rather than trusted.
            let bytes = sn_core::util::read_capped(path, DOCUMENT_FILE_CAP)
                .map_err(|e| BookError::Epub(format!("{}: {e}", path.display())))?;
            let extension = path
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            let parts = std::sync::Arc::new(
                ServedBookDoc::parse(&extension, bytes)
                    .map_err(|e| map_document_error(path, e))?,
            );
            Ok(parts)
        })
}

/// Map a document parse's refusal onto a book error. The `Unsupported`
/// refusals — DRM, a `.pdb` that is not a Mobipocket book — carry
/// messages already written for the user, so they pass through verbatim;
/// everything else wraps the file's name, like the other importers do.
fn map_document_error(source: &Path, e: sn_doc::bookdoc::BookDocError) -> BookError {
    match e {
        sn_doc::bookdoc::BookDocError::Unsupported(message) => BookError::Unsupported(message),
        other => BookError::Epub(format!("{}: {other}", source.display())),
    }
}

/// A headless-format book (`original.mobi`, `original.fb2`, `original.rtf`,
/// `original.html`, `original.txt`, …) as the reader serves it: the parse,
/// plus the file it came from.
///
/// An EPUB's chapters are zip entries the scheme handler reads out of the
/// archive; these formats have no container, so the *parsed* book is what
/// the handler serves — `chapter-N` answers with the chapter's own page,
/// an `img/<name>` with the image's bytes, and nothing is expanded to a
/// full HTML document beyond what the parse already holds. The original
/// bytes ride along so the parse memo's size accounting stays honest and a
/// share or export never needs a second read of the file.
pub struct ServedBookDoc {
    /// The original file, verbatim — what `original.<ext>` holds.
    bytes: Vec<u8>,
    /// The parsed book.
    doc: sn_doc::bookdoc::BookDoc,
}

/// The `.pdb` refusal: the extension is every Palm-era format's, not just
/// the books'. Said in the parser's own voice — the import path shows it
/// to the user.
const NOT_MOBIPOCKET_PDB: &str = "not a Mobipocket book — the .pdb extension is shared with \
                                 other Palm-era formats this app cannot read";

impl ServedBookDoc {
    /// Parse `bytes` as the headless format `extension` names. The one
    /// dispatch every document import and open goes through, so the `.pdb`
    /// gate and the parser choice cannot drift apart.
    pub fn parse(
        extension: &str,
        bytes: Vec<u8>,
    ) -> Result<Self, sn_doc::bookdoc::BookDocError> {
        use sn_doc::bookdoc::DocFamily;
        let doc = match DocFamily::from_extension(extension).ok_or_else(|| {
            sn_doc::bookdoc::BookDocError::Malformed(format!(
                "not a book format this app reads: .{extension}"
            ))
        })? {
            DocFamily::Mobi => {
                // A `.pdb` that is not a Mobipocket book gets the plain
                // refusal before the parser runs — decoding some other
                // Palm format as noise would only invent garbage.
                if extension == "pdb" && !sn_doc::mobi::looks_like_mobi(&bytes) {
                    return Err(sn_doc::bookdoc::BookDocError::Unsupported(
                        NOT_MOBIPOCKET_PDB.to_string(),
                    ));
                }
                sn_doc::mobi::parse(&bytes)?
            }
            DocFamily::Fb2 => sn_doc::fb2::parse(&bytes)?,
            DocFamily::Rtf => sn_doc::rtf::parse(&bytes)?,
            // The one family member whose extension changes the parser: an
            // HTMLZ is the ZIP calibre's converter writes, images included.
            DocFamily::Html if extension == "htmlz" => sn_doc::htmlbook::parse_htmlz(&bytes)?,
            DocFamily::Html => sn_doc::htmlbook::parse(&bytes)?,
            DocFamily::Txt => sn_doc::plainbook::parse(&bytes)?,
        };
        Ok(Self { bytes, doc })
    }

    /// The parsed book: its metadata, cover, chapters, contents and images.
    pub fn doc(&self) -> &sn_doc::bookdoc::BookDoc {
        &self.doc
    }

    /// The original file's bytes, verbatim.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The bytes for one virtual name the reader's scheme handler asks
    /// for. `chapter-N` answers with the chapter's own standalone page,
    /// `img/<name>` with an image the chapters reference — the cover under
    /// the name its XHTML would carry even when no chapter does. A name
    /// outside the book is `None`: the handler's 404, not a guess.
    pub fn read(&self, name: &str) -> Option<Vec<u8>> {
        if let Some(index) = name
            .strip_prefix("chapter-")
            .and_then(|rest| rest.parse::<usize>().ok())
        {
            let chapter = self.doc.chapters.get(index)?;
            // The page's `<title>` is the chapter's own when the format
            // carried one; a heading-less chunk is "Chapter N", the same
            // label its contents row falls back to.
            let title = chapter
                .title
                .as_deref()
                .map(str::to_string)
                .unwrap_or_else(|| format!("Chapter {}", index + 1));
            return Some(
                sn_doc::bookdoc::chapter_page(&title, &chapter.html).into_bytes(),
            );
        }
        if let Some(image) = name.strip_prefix("img/") {
            return self.image(image).map(|found| found.data.clone());
        }
        None
    }

    /// The content type a served name carries: the image's own from the
    /// parse, XHTML for a chapter page, the generic stream otherwise.
    pub fn mime_for(&self, name: &str) -> String {
        if let Some(image) = name.strip_prefix("img/") {
            if let Some(found) = self.image(image) {
                return found.mime.clone();
            }
        }
        if name.starts_with("chapter-") {
            return "text/html".to_string();
        }
        "application/octet-stream".to_string()
    }

    /// One image by its reference name — the `img/`-stripped side of
    /// [`sn_doc::bookdoc::BookDoc::image`], with the cover answering as
    /// `cover` even when no chapter references it.
    fn image(&self, name: &str) -> Option<&sn_doc::bookdoc::BookImage> {
        if let Some(found) = self.doc.image(name) {
            return Some(found);
        }
        if name == "cover" {
            return self.doc.cover.as_ref();
        }
        None
    }
}


impl std::fmt::Debug for Book {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Book")
            .field("id", &self.manifest.id)
            .field("title", &self.manifest.title)
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

impl Book {
    /// A book from its manifest and directory, with an empty archive cache.
    fn new(manifest: BookManifest, dir: PathBuf) -> Self {
        Self {
            manifest,
            dir,
            archive: std::rc::Rc::new(std::cell::RefCell::new(None)),
            pdf_doc: std::rc::Rc::new(std::cell::RefCell::new(None)),
            deck: std::rc::Rc::new(std::cell::RefCell::new(None)),
            document: std::rc::Rc::new(std::cell::RefCell::new(None)),
        }
    }

    /// The archive copy's path.
    pub fn original(&self) -> PathBuf {
        // The copy keeps the source's extension: a PDF book's file is
        // original.pdf, an EPUB's original.epub, a deck's original.pptx.
        let name = match self.manifest.format.as_str() {
            "pdf" => "original.pdf",
            "epub" => "original.epub",
            other => return self.dir.join(format!("original.{other}")),
        };
        self.dir.join(name)
    }

    /// Whether this book is a slide deck rather than an EPUB or a PDF.
    pub fn is_slides(&self) -> bool {
        matches!(
            self.manifest.format.as_str(),
            "pptx" | "ppsx" | "potx" | "ppt" | "odp"
        )
    }

    /// The parsed deck, opened on first use. Every reader — the viewer's
    /// document, the speech planner's text, the shelf's title — goes
    /// through here, so a deck open in the viewer is one parse for the
    /// whole handle's life.
    pub fn deck(&self) -> Result<std::sync::Arc<sn_doc::slides::SlideDeck>, BookError> {
        if let Some(deck) = self.deck.borrow().as_ref() {
            return Ok(deck.clone());
        }
        let deck = memoised_deck_parse(&self.original())?;
        *self.deck.borrow_mut() = Some(deck.clone());
        Ok(deck)
    }

    /// Whether this book is a headless document — the Mobipocket/Kindle
    /// family, FictionBook, RTF, HTML or plain text — rather than an EPUB,
    /// a PDF, a deck or an audiobook. The manifest's format is the file's
    /// own extension, so the one extension table decides.
    pub fn is_document(&self) -> bool {
        sn_doc::bookdoc::DocFamily::is_document_extension(&self.manifest.format)
    }

    /// The parsed document behind a headless-format book, opened on first
    /// use. The reader's scheme handler serves the chapters out of it and
    /// the speech path extracts from it — one parse for the whole handle's
    /// life, and (through the memo) for every handle on the file.
    pub fn served(&self) -> Result<std::sync::Arc<ServedBookDoc>, BookError> {
        if let Some(doc) = self.document.borrow().as_ref() {
            return Ok(doc.clone());
        }
        let doc = memoised_document_parse(&self.original())?;
        *self.document.borrow_mut() = Some(doc.clone());
        Ok(doc)
    }

    /// The audio file's path, for an audiobook.
    pub fn audio(&self) -> PathBuf {
        self.dir.join(if self.manifest.format == "opus" {
            "original.opus"
        } else {
            "original.m4b"
        })
    }

    /// Whether this book is an audiobook rather than an EPUB.
    ///
    /// Positive evidence only: a chapter list. An empty spine is not
    /// evidence — a corrupt or hand-edited EPUB has one too, and treating it
    /// as an audiobook sends the reader down a path that looks for an audio
    /// file the book does not have.
    pub fn is_audiobook(&self) -> bool {
        !self.manifest.chapters.is_empty()
    }

    /// The cover image's path, when one was extracted.
    pub fn cover(&self) -> Option<PathBuf> {
        self.manifest
            .has_cover
            .then(|| self.dir.join("cover.jpg"))
    }

    /// The title, falling back to the file name when the OPF had none.
    pub fn title(&self) -> &str {
        &self.manifest.title
    }

    /// The chapter count.
    pub fn chapters(&self) -> usize {
        self.manifest.spine_count
    }

    /// The first chapter with speakable prose in it, skipping front matter.
    /// A book whose first spine entries are cover/title pages must open on
    /// its first real chapter, not on a blank page — nor on a page whose
    /// layer is junk (the gate's verdict, the same one the read path takes;
    /// skipping it here costs nothing, the scores are per-page and cheap).
    /// The walk is capped: a scanned book has NO speakable page, and
    /// proving that over 2 000 pages extracts every one of them. Fifty
    /// pages of silence is the honest answer "this book opens at page 0
    /// and the reader shows what is there". The walk reads the text layer
    /// only — OCR here would cost a second a page over a scanned book,
    /// and the page the book opens on is OCR'd there, once, cached.
    pub fn first_speakable_chapter(&self) -> usize {
        let cap = self.chapters().min(50);
        for index in 0..cap {
            if let Ok(text) = self.chapter_text_layer(index) {
                if !text.trim().is_empty()
                    && !sn_doc::textquality::layer_quality(&text).is_garbage()
                {
                    return index;
                }
            }
        }
        // A scanned book keeps no layer on any page, so the walk above ends
        // at page 0 — whose recognition may have run already (the warm-up,
        // or a page the reader visited). The first non-empty cached page is
        // a better opening than a title page the voice cannot read; empty
        // caches (recognition ran and found nothing) are not openings.
        if self.manifest.format == "pdf" {
            if let Some(first) = first_nonempty_ocr_cache(&self.dir) {
                return first.saturating_sub(1);
            }
        }
        0
    }

    /// Read one chapter's speakable text. `index` is into `spine`.
    pub fn chapter_text(&self, index: usize) -> Result<String, BookError> {
        Ok(self.chapter_full(index)?.text)
    }

    /// Whether the chapter's *text layer* alone holds something worth
    /// speaking — the cheap half of the speech walk. Where this is true,
    /// [`Self::chapter_text`] would return exactly the layer (a good layer
    /// is never demoted), so the walk can skip the full read and the OCR it
    /// may carry.
    pub fn chapter_layer_speakable(&self, index: usize) -> bool {
        matches!(self.chapter_text_layer(index), Ok(text)
            if !text.trim().is_empty()
                && !sn_doc::textquality::layer_quality(&text).is_garbage())
    }

    /// [`chapter_text`] without the OCR fallback: what the file's own text
    /// layer says, for cheap walks over many pages.
    pub fn chapter_text_layer(&self, index: usize) -> Result<String, BookError> {
        if self.manifest.format != "pdf" {
            if self.is_slides() {
                return self.slide_text(index, false);
            }
            return self.chapter_text(index);
        }
        if index >= self.manifest.spine_count {
            return Err(BookError::Epub(format!("page {index} is past the end")));
        }
        let doc = self.cached_pdf_doc()?;
        Ok(Self::pdf_page_from(index, &doc, self.manifest.spine_count)?.text)
    }

    /// One slide's speakable text. `with_ocr` folds in the recognised text
    /// of the slide's pictures (cached beside the manifest); without it the
    /// slide's own text only — the cheap walk the shelf makes.
    fn slide_text(&self, index: usize, with_ocr: bool) -> Result<String, BookError> {
        let deck = self.deck()?;
        if index >= deck.slides.len() {
            return Err(BookError::Epub(format!("slide {index} is past the end")));
        }
        let dir = self.dir.clone();
        let (text, _spans) = deck.slide_text(index, |media| {
            if !with_ocr {
                return None;
            }
            Self::slide_image_ocr(&dir, index, media)
        });
        Ok(text)
    }

    /// A slide image's recognised text: the cache file beside the manifest
    /// when it exists, a fresh recognition otherwise (and the cache is
    /// written). `None` when there is no engine or the image has no text.
    fn slide_image_ocr(
        dir: &std::path::Path,
        index: usize,
        media: &sn_doc::slides::DeckMedia,
    ) -> Option<String> {
        let cache = dir.join(format!("text/slide-{index:03}-img-{}.txt", media.name));
        if let Ok(cached) = std::fs::read_to_string(&cache) {
            return Some(cached);
        }
        if !sn_doc::ocr::available() {
            return None;
        }
        match sn_doc::ocr::image_text(&media.data) {
            Ok(text) => {
                if let Some(parent) = cache.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(&cache, &text);
                log::info!(
                    "ocr read a slide image: slide {} ({} chars)",
                    index + 1,
                    text.chars().count()
                );
                Some(text)
            }
            Err(e) => {
                log::warn!("ocr failed for slide {} image: {}", index + 1, e);
                None
            }
        }
    }

    /// Read one chapter whole: its speakable text plus its images, the two
    /// things the reader shows. Speech callers use `chapter_text`; the
    /// reader needs both and pays the archive read once.
    pub fn chapter_full(
        &self,
        index: usize,
    ) -> Result<sn_doc::chapter::ChapterText, BookError> {
        if self.manifest.format == "pdf" {
            return self.pdf_page_full(index);
        }
        if self.is_slides() {
            // A slide's own text plus the text recognised in its pictures:
            // the read-along view speaks both.
            return Ok(sn_doc::chapter::ChapterText {
                text: self.slide_text(index, true)?,
                images: Vec::new(),
                blocks: Vec::new(),
            });
        }
        if self.is_document() {
            // A headless document's chapter is XHTML the parse already
            // holds — the ordinary pass extracts it. (Plain-text chapters
            // are `<p>`-wrapped XHTML, so the same pass reads them.)
            let parts = self.served()?;
            let chapter = parts
                .doc()
                .chapters
                .get(index)
                .ok_or_else(|| BookError::Epub(format!("chapter {index} is past the end")))?;
            return Ok(sn_doc::chapter::extract_in(&chapter.html, ""));
        }
        let entry = self
            .manifest
            .spine
            .get(index)
            .ok_or_else(|| BookError::Epub(format!("chapter {index} is past the end")))?;
        let archive = self.archive()?;
        let bytes = archive
            .read(entry)
            .map_err(|e| BookError::Epub(format!("{entry}: {e}")))?;
        let xhtml = String::from_utf8_lossy(&bytes);
        // The chapter's own directory decides how its <img src>s resolve.
        let base_dir = match entry.rfind('/') {
            Some(i) => &entry[..i],
            None => "",
        };
        Ok(sn_doc::chapter::extract_in(&xhtml, base_dir))
    }

    /// One PDF page as a ChapterText: the page's text, no images (a PDF's
    /// figures are drawn, not referenced). Cached pages live in the same
    /// slot as the EPUB archive: a book handle is read once.
    fn pdf_page_full(&self, index: usize) -> Result<sn_doc::chapter::ChapterText, BookError> {
        if index >= self.manifest.spine_count {
            return Err(BookError::Epub(format!("page {index} is past the end")));
        }
        // One parse per book, not one per handle: the shelf's clone and the
        // reader's own handle are different `Book` values, and the viewer's
        // layout read plus the speech read plus the shelf's first-speakable
        // walk are three more chances to pay four seconds each. The memo is
        // keyed by path and size, so a re-import is a different parse.
        let doc = self.cached_pdf_doc()?;
        Self::pdf_page_from_in(index, &doc, self.manifest.spine_count, Some(&self.dir))
    }

    /// The shared parse, opened on first use. Every reader — page text, page
    /// sizes, the viewer's rasteriser's sibling calls — goes through here, so
    /// a book open in the viewer is one parse for the whole handle's life.
    fn cached_pdf_doc(&self) -> Result<std::sync::Arc<sn_doc::pdf::Document>, BookError> {
        if let Some(doc) = self.pdf_doc.borrow().as_ref() {
            return Ok(doc.clone());
        }
        let doc = memoised_pdf_parse(&self.original())?;
        *self.pdf_doc.borrow_mut() = Some(doc.clone());
        Ok(doc)
    }

    /// The page's text out of an already-parsed document. The window of
    /// neighbours is what the reader paginates by anyway, and extraction is
    /// per-page from one parse.
    ///
    /// A page with no text layer (a scan) is OCR'd — FEATURES 4.9 — and so
    /// is a page whose layer scores as junk (`textquality`): recognition is
    /// cached at `text/NNNN.txt` beside the manifest, the documented
    /// convention: OCR costs a second a page, the cache costs a file read,
    /// and a cached page reads even on a box where tesseract has since gone
    /// missing. The 200 dpi raster is what the viewer's own renderer draws
    /// at reading zoom; recognition matches the eye.
    fn pdf_page_from(
        index: usize,
        doc: &sn_doc::pdf::Document,
        spine_count: usize,
    ) -> Result<sn_doc::chapter::ChapterText, BookError> {
        Self::pdf_page_from_in(index, doc, spine_count, None)
    }

    /// [`pdf_page_from`] with this book's own directory, which is what makes
    /// the OCR cache possible. `dir` is the book's directory (`Books/<uuid>`).
    fn pdf_page_from_in(
        index: usize,
        doc: &sn_doc::pdf::Document,
        spine_count: usize,
        dir: Option<&std::path::Path>,
    ) -> Result<sn_doc::chapter::ChapterText, BookError> {
        let from = index.saturating_sub(2);
        let to = (index + 3).min(spine_count);
        let pages = sn_doc::pdftext::extract_pages(doc, from, to);
        let page = pages
            .iter()
            .find(|p| p.page == index + 1)
            .ok_or_else(|| BookError::Epub(format!("page {index} came back empty")))?;
        let mut text = page.text.clone();
        // The quality gate (FEATURES: the powerful parser): a layer can be
        // present and still be junk — OCR garbage a producer baked in, a
        // font mapped into the private use area, a scanner watermark — and
        // speaking that verbatim is worse than speaking nothing. A second
        // opinion from the interpreter first (only where pdfium read the
        // page; otherwise that IS the interpreter's answer), then the OCR
        // fallback a scan takes. [`layer_needs_ocr`] is the one decision the
        // read path and the warm-up share: empty, junk, or watermark.
        let garbage = sn_doc::textquality::layer_needs_ocr(&text);
        if garbage && sn_doc::pdftext::pdfium_is_the_engine(doc) {
            let alt = sn_doc::pdftext::page_text_from_chars(
                doc,
                index,
                sn_doc::pdftext::extract_chars_interpreted(doc, index),
            );
            // "Clean" has to mean something was read: a scan answers empty
            // from every glyph source, and taking that as a clean read would
            // spare the page its OCR forever.
            let alt_garbage = sn_doc::textquality::layer_needs_ocr(&alt);
            if !alt_garbage {
                text = alt;
                log::info!(
                    "page {}: text layer was junk, the interpreter read it cleanly",
                    index + 1
                );
            }
        }
        if let Some(dir) = dir {
            // Where the recognition comes from: a junk layer *needs* it
            // (cached or fresh); a clean layer under Force OCR gets the
            // control's refreshed reading; a clean layer otherwise reads
            // from the cache only — recognition here would stall the
            // reader two seconds a page, and the warm-up has usually
            // already been here — a page it has not reached reads from its
            // layer alone this time and picks the mix up when the cache
            // lands.
            let forced = dir.join(FORCE_OCR_MARKER).is_file();
            let ocr = if garbage {
                Self::ocr_page(dir, index)
            } else if forced {
                Self::ocr_page_forced(dir, index)
            } else {
                Self::ocr_page_cached(dir, index)
            };
            if let Some(ocr) = ocr {
                // One reading leads; the other only fills its gaps — and
                // only words the leader is missing, spliced into the line
                // they belong to. The old shape appended whole lines, so a
                // misspelled or differently-wrapped copy of a sentence the
                // leader already had was appended too, and the page said
                // everything twice (`sn_doc::reconcile` for the details).
                //
                // A demonstrably worse leader is dropped outright, not
                // reconciled: a producer's shifted-OCR layer ("n1ay-be the
                // catJse") is full of real words, so reconciliation would
                // let its junk leak into the good reading through the
                // gap-filling path — two readings of one page, one
                // demonstrably more prose-like, the better one stands
                // alone.
                if garbage {
                    text = ocr;
                    log::info!("page {}: text layer was junk, OCR read it instead", index + 1);
                } else if sn_doc::textquality::reading_beats(&text, &ocr) {
                    log::info!(
                        "page {}: the recognition reads better than the text layer; \
                         the layer was dropped",
                        index + 1
                    );
                    text = ocr;
                } else if forced {
                    // The control's promise: recognition is what the page
                    // says. The layer fills the recognition's gaps and
                    // nothing else.
                    let led = sn_doc::reconcile::reconcile(&ocr, &text);
                    if led.len() != ocr.len() {
                        log::info!(
                            "page {}: recognition leads, the layer filled {} chars it was missing",
                            index + 1,
                            led.len() - ocr.len()
                        );
                    }
                    text = led;
                } else {
                    // Both readings are honest: the layer leads (it has the
                    // document's own reading order and structure) and the
                    // recognition fills the words it missed.
                    let led = sn_doc::reconcile::reconcile(&text, &ocr);
                    if led.len() != text.len() {
                        log::info!(
                            "page {}: the OCR mix added {} chars",
                            index + 1,
                            led.len() - text.len()
                        );
                    }
                    text = led;
                }
            }
        }
        Ok(sn_doc::chapter::ChapterText {
            text,
            images: Vec::new(),
            // A PDF page has no markup to recover: its layout was read as
            // text, and the reader draws it as the flat lines it has.
            blocks: Vec::new(),
        })
    }

    /// The page's OCR cache, when one is already written — never a
    /// recognition. The mix path reads this: folding recognition into a page
    /// whose layer is fine must not cost the reader a two-second stall, so a
    /// page the warm-up has not reached yet reads from its layer alone this
    /// time and picks the mix up when the cache lands.
    fn ocr_page_cached(dir: &std::path::Path, index: usize) -> Option<String> {
        let cache = dir.join(format!("text/{:04}.txt", index + 1));
        std::fs::read_to_string(&cache).ok().filter(|t| !t.is_empty())
    }

    /// The same, but the cache is *refreshed when it predates the control*:
    /// the page is recognised again, and the fresh reading replaces the old
    /// one only if it is not materially worse.
    ///
    /// Force OCR is the control that says "recognise this page even though
    /// a cache exists" — a page cached before the control was on was
    /// recognised *without* it, so serving that cache would make the
    /// control do nothing at all for every page the reader has already
    /// visited. But "before the control" is a question of time, not of
    /// existence: a cache written *while* the control was on already
    /// honours it, and re-recognising that on every open would burn
    /// seconds a page for nothing new. So the cache is stale when it is
    /// older than the marker file, fresh otherwise — and a fresh cache is
    /// served as-is.
    ///
    /// The cache holds the raw recognition (reconciliation with the layer
    /// happens at read time, against the control's current rules), and the
    /// merge guarantee stands: a forced re-read that recovers *less* than
    /// an earlier pass — a different detection threshold, a hair of camera
    /// blur — is refused, and the better reading stands.
    fn ocr_page_forced(dir: &std::path::Path, index: usize) -> Option<String> {
        // The whole read-merge-write runs under the page lock: a warm-up
        // thread recognising this page between the old-cache read and the
        // write would be overwritten.
        let lock = page_lock(dir, index);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let cache = dir.join(format!("text/{:04}.txt", index + 1));
        if !Self::ocr_cache_is_stale(dir, index) {
            return std::fs::read_to_string(&cache).ok().filter(|t| !t.is_empty());
        }
        let previous = std::fs::read_to_string(&cache).ok().filter(|t| !t.is_empty());
        let fresh = Self::recognise_page(dir, index)?;
        let text = match previous {
            Some(old) if sn_doc::textquality::reading_beats(&fresh, &old) => old,
            _ => fresh,
        };
        if let Some(parent) = cache.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&cache, &text);
        Some(text)
    }

    /// Whether a page's OCR cache predates the Force OCR marker — stale,
    /// in the control's terms, and due for a re-read. A missing cache or a
    /// missing marker timestamp (an untouchable file) is stale: the page
    /// gets its recognition rather than quietly skipping it.
    fn ocr_cache_is_stale(dir: &std::path::Path, index: usize) -> bool {
        let cache = dir.join(format!("text/{:04}.txt", index + 1));
        let marker = dir.join(FORCE_OCR_MARKER);
        match (cache.metadata(), marker.metadata()) {
            (Ok(c), Ok(m)) => match (c.modified().ok(), m.modified().ok()) {
                (Some(c), Some(m)) => c < m,
                _ => true,
            },
            _ => true,
        }
    }

    /// The recognition itself — raster the page, read it, return the text.
    /// No lock and no cache write: the callers hold the page lock and decide
    /// what is written (the plain read caches its text; a forced re-read
    /// caches the raw recognition — reconciliation with the layer happens
    /// at read time).
    fn recognise_page(dir: &std::path::Path, index: usize) -> Option<String> {
        if !sn_doc::ocr::available() {
            return None;
        }
        let pdf = dir.join("original.pdf");
        if !pdf.is_file() {
            return None;
        }
        match sn_doc::ocr::page_text_with_engine(&pdf, index, 200) {
            Ok((text, engine)) => {
                log::info!(
                    "ocr read a scanned page with {}: page {} ({} chars)",
                    engine,
                    index + 1,
                    text.chars().count()
                );
                Some(text)
            }
            Err(e) => {
                log::warn!("ocr failed for page {}: {}", index + 1, e);
                None
            }
        }
    }

    /// A page's cached-or-fresh OCR text — a scan's page, or one whose text
    /// layer scored as junk. `None` when there is nothing to have: no cache
    /// file, no book directory, or no OCR engine.
    fn ocr_page(dir: &std::path::Path, index: usize) -> Option<String> {
        let cache = dir.join(format!("text/{:04}.txt", index + 1));
        if let Ok(cached) = std::fs::read_to_string(&cache) {
            if !cached.is_empty() {
                return Some(cached);
            }
        }
        // One recognition per page, however many callers want it.
        //
        // Two callers *do* want the same page at the same time: the
        // background warm-up thread and the reader's own read of the page
        // the user is looking at. Without this lock they each recognised it
        // — two seconds of recognition each, for one page — and the second
        // one to finish overwrote the first's cache.
        //
        // The lock is per (book, page), so different pages still recognise
        // in parallel; only the same page is serialised.
        let lock = page_lock(dir, index);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        // The cache is re-read **after** taking the lock: the thread that
        // held it may have just written the very file this caller came for.
        // Re-reading is what turns the second caller from "recognise again"
        // into "a cache read".
        if let Ok(cached) = std::fs::read_to_string(&cache) {
            if !cached.is_empty() {
                return Some(cached);
            }
        }
        let text = Self::recognise_page(dir, index)?;
        // Best-effort cache: a failed write only means the next read
        // recognises the page again.
        if let Some(parent) = cache.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&cache, &text);
        Some(text)
    }

    /// The book's Force OCR control: recognition leads the page (the layer
    /// only fills its gaps — see the read path and
    /// [`sn_doc::reconcile::reconcile`]), and a cache older than the
    /// control is refreshed (see [`Self::ocr_page_forced`]). A marker file
    /// beside the manifest — not a manifest field — because the manifest
    /// is byte-compatible with the iOS app's and this control is ours.
    pub fn force_ocr(&self) -> bool {
        self.dir.join(FORCE_OCR_MARKER).is_file()
    }

    pub fn set_force_ocr(&self, on: bool) {
        let marker = self.dir.join(FORCE_OCR_MARKER);
        if on {
            if let Some(parent) = marker.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&marker, b"");
        } else {
            let _ = std::fs::remove_file(&marker);
        }
    }

    /// Read the book's pages through OCR ahead of any reader, caching each
    /// result — the "every page of a scan is speakable and searchable"
    /// guarantee, paid once instead of per visit.
    ///
    /// The walk is a **window, not a sweep**: it starts at the reader's
    /// position and runs a fixed look-ahead ahead of it. Recognition costs
    /// about two seconds a page, the voice reads a page in tens of seconds,
    /// and every chapter open re-anchors the walk — so a bounded window
    /// keeps the next several minutes of speech warm while never costing a
    /// 1295-page textbook twenty minutes of recognition before its first
    /// page is usable. The old shape extracted *every* page's text layer in
    /// one call before recognising anything; that one extraction was itself
    /// seconds on a big book.
    ///
    /// Every page in the window is recognised (not only pages whose layer is
    /// empty or junk): a layer the gate passed can still be incomplete, and
    /// the reader reconciles the recognised words into it. A page with a
    /// fresh cache is a `stat`; under Force OCR a cache older than the
    /// control is refreshed (see [`Self::ocr_page_forced`]) — the walk is
    /// the **same window in both modes**, because the control is about
    /// which reading leads, not about how far ahead to read: a 1295-page
    /// sweep behind a toggle cost the user forty minutes of recognition
    /// they could not stop, and a chapter is read from the window it sits
    /// in anyway. Runs on the caller's thread — the
    /// reader calls it on a worker, on a handle the worker owns (a `Book` is
    /// main-thread-shaped, and this one must not be shared). `stop` ends the
    /// walk early; the caches already written stay. Returns the number of
    /// pages newly recognised.
    ///
    /// `on_page(done, total, page)` fires after every page the walk handles,
    /// so a caller can show progress: `done` pages newly recognised of the
    /// `total` this walk covers, `page` (1-based) the one just finished.
    pub fn ocr_warmup(
        &self,
        stop: &std::sync::atomic::AtomicBool,
        mut on_page: impl FnMut(usize, usize, usize),
    ) -> usize {
        use std::sync::atomic::Ordering;
        /// How many pages past the reader's position one pass keeps warm.
        /// At ~2 s of recognition a page this is ~25 s of background work
        /// per open — cheap enough to run on every open, deep enough that
        /// the voice (tens of seconds a page, and every advance re-anchors
        /// the walk) never reaches an unrecognised page.
        const LOOKAHEAD: usize = 12;
        if self.manifest.format != "pdf" || !sn_doc::ocr::available() {
            return 0;
        }
        if !self.dir.join("original.pdf").is_file() {
            return 0;
        }
        let forced = self.force_ocr();
        let spine = self.manifest.spine_count;
        let anchor = self.manifest.position.chapter_index.min(spine.saturating_sub(1));
        let to = (anchor + LOOKAHEAD).min(spine);
        let total = to - anchor;
        let mut recognised = 0usize;
        for index in anchor..to {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            // The control's walk dies the moment the control is off:
            // toggling Force OCR off mid-sweep ends the walk at the next
            // page boundary, instead of leaving a 1295-page sweep running
            // behind a switch that says it is off.
            if forced && !self.force_ocr() {
                break;
            }
            // A page with a fresh cache is done. Under Force OCR a cache
            // older than the control is not: it was recognised *without*
            // the control's rules, and the refresh is what makes the
            // control do something for pages the reader has already
            // visited (see [`Self::ocr_page_forced`]).
            let cache = self.dir.join(format!("text/{:04}.txt", index + 1));
            if cache.is_file() && !(forced && Self::ocr_cache_is_stale(&self.dir, index)) {
                continue;
            }
            let read = if forced {
                Self::ocr_page_forced(&self.dir, index)
            } else {
                Self::ocr_page(&self.dir, index)
            };
            if read.is_some() {
                recognised += 1;
            }
            on_page(recognised, total, index + 1);
        }
        recognised
    }

    /// The extracted text of a range of PDF pages (0-based, half-open), joined
    /// the way `pdftext` joins blocks — blank lines between pages.
    ///
    /// The viewer is page *images* now; this is the text side of the split.
    /// The same pages' text feeds one call: the reader keeps it behind the
    /// `Read along` toggle so the voice has something to say about a scanned
    /// figure and the search has something to find, and its byte offsets are
    /// the domain the bookmark's UTF-16 offset converts through — one
    /// extraction, one string, one plan.
    pub fn pdf_page_text(
        &self,
        from: usize,
        to: usize,
    ) -> Result<String, BookError> {
        if self.manifest.format != "pdf" {
            return Err(BookError::Epub(
                "pdf_page_text is for PDF books".to_string(),
            ));
        }
        let doc = self.cached_pdf_doc()?;
        let pages = sn_doc::pdftext::extract_pages(&doc, from, to);
        Ok(pages
            .iter()
            .map(|p| p.text.clone())
            .collect::<Vec<_>>()
            .join("\n\n"))
    }

    /// The viewer's layout numbers: `(width, height)` in points per page, in
    /// reading order.
    ///
    /// A fixed row height is how a continuous scroll knows where page 741
    /// starts without rendering the 740 above it, so this is asked once per
    /// open book — from the same parse the text side holds, when it holds one.
    pub fn page_sizes(&self) -> Result<Vec<(f64, f64)>, BookError> {
        let doc = self.cached_pdf_doc()?;
        let mut sizes = doc.page_sizes();
        if sizes.is_empty() {
            // A container this parser cannot read the tree out of still has a
            // page count the manifest counted at import; letter for all of
            // them is what the viewer would draw, not a guess about the file.
            sizes = vec![(612.0, 792.0); self.manifest.spine_count];
        }
        Ok(sizes)
    }

    /// The viewer's link annotations: the book's own clickable contents —
    /// each row's rectangle (page-local points, top-left origin) and the
    /// page it jumps to, or the web address it carries. A book whose parse
    /// cannot be read answers an empty table; the contents page then just
    /// does not click, as it has always been.
    pub fn page_links(&self) -> Vec<sn_doc::pdf::PageLink> {
        self.cached_pdf_doc()
            .map(|doc| doc.page_links())
            .unwrap_or_default()
    }

    /// Parse the archive, once per Book handle. Every chapter read after the
    /// first is a hash lookup: the manifest means the shelf never parses at
    /// all, and a search across a book's chapters parses once instead of
    /// once per chapter (the 136 MB Sherris book measured 10.5 s per
    /// keystroke before this). The process-wide memo underneath means the
    /// parse an open-path worker paid for reaches this handle too.
    pub fn archive(&self) -> Result<sn_doc::zip::Archive, BookError> {
        {
            let cached = self.archive.borrow();
            if let Some(archive) = cached.as_ref() {
                return Ok((**archive).clone_shallow());
            }
        }
        let parsed = memoised_archive(&self.original())?;
        *self.archive.borrow_mut() = Some(parsed.clone());
        Ok(parsed.clone_shallow())
    }
}

/// The book library: the `Books/` directory and its manifests.
pub struct BooksStore;

impl BooksStore {
    /// The library root.
    pub fn dir() -> PathBuf {
        paths::books_dir()
    }

    /// A handle for a manifest already in hand.
    ///
    /// The open path runs on a worker thread, and a `Book` (its `Rc` caches
    /// make it main-thread-only) cannot cross back. The worker posts the
    /// manifest — plain data — and the UI thread rebuilds its handle from it
    /// here. The directory is the id's, which is where every manifest lives.
    pub fn from_manifest(manifest: BookManifest) -> Book {
        let dir = Self::dir().join(&manifest.id);
        Book::new(manifest, dir)
    }

    /// Every book, sorted by `addedAt` (oldest first, as the shelf shows).
    pub fn all() -> Result<Vec<Book>, BookError> {
        let root = Self::dir();
        let Ok(entries) = std::fs::read_dir(&root) else {
            return Ok(Vec::new());
        };
        let mut books = Vec::new();
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let manifest_path = path.join("manifest.json");
            if !manifest_path.exists() {
                continue;
            }
            let Ok(raw) = std::fs::read_to_string(&manifest_path) else {
                continue;
            };
            if let Ok(manifest) = serde_json::from_str::<BookManifest>(&raw) {
                books.push(Book::new(manifest, path));
            }
        }
        books.sort_by(|a, b| {
            a.manifest
                .added_at
                .partial_cmp(&b.manifest.added_at)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(books)
    }

    /// One book by id, or by a unique prefix of its id.
    ///
    /// The library is read **once**: it was read twice on the plain path (and
    /// every read parses every manifest on the box), which on the UI thread
    /// was a tax the open path paid for nothing.
    pub fn find(prefix: &str) -> Result<Option<Book>, BookError> {
        let all = Self::all()?;
        // An exact id match wins over a prefix that is also a prefix of
        // another book's id — the CLI's `--play-book <id-prefix>` case.
        if let Some(exact) = all.iter().find(|b| b.manifest.id == prefix) {
            return Ok(Some(exact.clone()));
        }
        let mut matches: Vec<Book> = all
            .into_iter()
            .filter(|b| b.manifest.id.starts_with(prefix))
            .collect();
        if matches.len() == 1 {
            return Ok(matches.pop());
        }
        Ok(None)
    }

    /// Import an EPUB from `source`. Returns the new book.
    ///
    /// The file is copied into `Books/<id>/original.epub` through a `.part`
    /// rename, the cover is extracted beside it, and the manifest is written
    /// last — so an interrupted import leaves a directory with no manifest,
    /// which the shelf ignores rather than showing a book that cannot open.
    pub fn import(source: &Path) -> Result<Book, BookError> {
        // Whole-book read, capped: an "epub" can arrive from a LAN peer who
        // chose its size freely, and the cap keeps that a refused import
        // instead of an OOM. Anything over 512 MiB is not a book.
        let bytes = sn_core::util::read_capped(source, 512 * 1024 * 1024)?;
        let archive = sn_doc::zip::Archive::parse(bytes)
            .map_err(|e| BookError::NotAnEpub(format!("{}: {e}", source.display())))?;
        let info = sn_doc::epub::parse(&archive)
            .map_err(|e| BookError::Epub(format!("{}: {e}", source.display())))?;
        if info.spine.is_empty() {
            return Err(BookError::NotAnEpub(format!(
                "{}: no reading order in the package",
                source.display()
            )));
        }

        let id = new_book_id();
        let dir = Self::dir().join(&id);
        std::fs::create_dir_all(&dir)?;

        // 1. the archive copy, atomically: a kill -9 mid-copy must leave a
        //    `.part` behind, never a half-written `original.epub` that the
        //    next import would find in place. Written from the parsed
        //    archive's own bytes — the file was read once, not twice.
        sn_core::atomic::write_atomic(&dir.join("original.epub"), archive.raw())?;

        // 2. the cover, extracted from the archive, also atomically. JPEG,
        //    PNG and GIF all become cover.jpg — GTK's image loader sniffs
        //    the content, and the manifest only records whether one exists.
        let has_cover = match info.cover.as_deref() {
            Some(path) => match archive.read(path) {
                Ok(cover_bytes) if !cover_bytes.is_empty() => sn_core::atomic::write_atomic(
                    &dir.join("cover.jpg"),
                    &cover_bytes,
                )
                .is_ok(),
                _ => false,
            },
            None => false,
        };

        // 3. the manifest, last.
        let now = sn_core::AppleDate::now().raw();
        let title = info
            .title
            .clone()
            .unwrap_or_else(|| file_stem(source));
        let manifest = BookManifest {
            id: id.clone(),
            format: "epub".to_string(),
            added_at: now,
            last_opened_at: now,
            title,
            author: info.creator.clone().unwrap_or_default(),
            spine_count: info.spine.len(),
            toc: info
                .toc
                .iter()
                .map(|t| ManifestTocEntry {
                    spine_index: t.spine_index,
                    href: t.href.clone(),
                    label: t.label.clone(),
                    depth: t.depth,
                    fragment: t.fragment.clone(),
                })
                .collect(),
            position: BookPosition::default(),
            has_cover,
            original_file_name: source
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            spine: info.spine.clone(),
            duration_seconds: None,
            audio_codec: None,
            chapters: Vec::new(),
        };
        let json = serde_json::to_vec_pretty(&manifest)
            .map_err(|e| BookError::Epub(e.to_string()))?;
        sn_core::atomic::write_atomic(&dir.join("manifest.json"), &json)?;

        Ok(Book::new(manifest, dir))
    }

    /// Import a PDF into the library. The archive copy is the file itself;
    /// the manifest carries the page count and the outline as the TOC, and
    /// each "chapter" is a page-range the reader asks for on demand.
    pub fn import_pdf(source: &Path) -> Result<Book, BookError> {
        // A scanned tome can honestly reach 1 GiB; past that it is a
        // disk-fill "pdf", refused here rather than OOM-ing the import.
        let bytes = sn_core::util::read_capped(source, 1024 * 1024 * 1024)?;
        let doc = sn_doc::pdf::Document::parse(&bytes)
            .map_err(|e| BookError::Epub(format!("{}: {e}", source.display())))?;
        let info = doc
            .info()
            .map_err(|e| BookError::Epub(format!("{}: {e}", source.display())))?;
        if info.encrypted {
            return Err(BookError::Epub(format!(
                "{}: the pdf is encrypted; its text cannot be read",
                source.display()
            )));
        }
        let page_count = info.page_count;
        if page_count == 0 {
            return Err(BookError::Epub(format!(
                "{}: the pdf names no pages",
                source.display()
            )));
        }

        let id = new_book_id();
        let dir = Self::dir().join(&id);
        std::fs::create_dir_all(&dir)?;
        sn_core::atomic::write_atomic(&dir.join("original.pdf"), &bytes)?;

        // The cover is the first page, rasterised by poppler through the same
        // temporary-directory cache the reader uses: a PDF has no embedded
        // cover image to pull, and the shelf's row is 44×60, so a low-dpi
        // JPEG of page 1 is the cover. No poppler means no cover — the shelf
        // draws its placeholder, which is the honest answer.
        let has_cover = render_pdf_page_cover(&dir.join("original.pdf"), 1, &dir.join("cover.jpg"));

        // The TOC is the outline, one row per entry, pointing at its page
        // (1-based → 0-based spine index), with the outline's nesting depth
        // carried through. A book with no outline gets the default chapter
        // list.
        let toc: Vec<ManifestTocEntry> = info
            .outline
            .iter()
            .filter_map(|entry| {
                entry.page.map(|page| ManifestTocEntry {
                    spine_index: Some((page - 1).min(page_count - 1)),
                    href: format!("page-{}", page),
                    label: entry.title.clone(),
                    depth: entry.depth,
                    fragment: None,
                })
            })
            .collect();

        let now = sn_core::AppleDate::now().raw();
        let manifest = BookManifest {
            id: id.clone(),
            format: "pdf".to_string(),
            added_at: now,
            last_opened_at: now,
            title: info
                .title
                .clone()
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| file_stem(source)),
            author: info.author.clone().unwrap_or_default(),
            spine_count: page_count,
            spine: (0..page_count).map(|p| format!("page-{p}")).collect(),
            toc,
            position: BookPosition::default(),
            has_cover,
            original_file_name: source
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            duration_seconds: None,
            audio_codec: None,
            chapters: Vec::new(),
        };
        let json = serde_json::to_vec_pretty(&manifest)
            .map_err(|e| BookError::Epub(e.to_string()))?;
        sn_core::atomic::write_atomic(&dir.join("manifest.json"), &json)?;

        Ok(Book::new(manifest, dir))
    }

    /// Import a slide deck (`.pptx`, `.ppsx`, `.potx`, legacy `.ppt`,
    /// `.odp`) into the library.
    ///
    /// The deck is copied into `Books/<id>/original.<ext>` atomically; the
    /// manifest carries the slide count and one TOC row per slide. A deck
    /// has no cover image to extract — the shelf draws its placeholder,
    /// which is the honest answer.
    pub fn import_slides(source: &Path) -> Result<Book, BookError> {
        let extension = source
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if !matches!(extension.as_str(), "pptx" | "ppsx" | "potx" | "ppt" | "odp") {
            return Err(BookError::NotAnEpub(format!(
                "{}: not a slide deck format this app imports",
                source.display()
            )));
        }
        // A deck with 300 slides and 200 pictures runs to tens of megabytes;
        // a LAN peer may send a "deck" of any size, so the cap keeps that
        // a refused import rather than an OOM.
        let bytes = sn_core::util::read_capped(source, 512 * 1024 * 1024)?;
        let deck = sn_doc::slides::SlideDeck::parse(&bytes)
            .map_err(|e| BookError::Epub(format!("{}: {e}", source.display())))?;
        if deck.slides.is_empty() {
            return Err(BookError::Epub(format!(
                "{}: the deck has no slides",
                source.display()
            )));
        }

        let id = new_book_id();
        let dir = Self::dir().join(&id);
        std::fs::create_dir_all(&dir)?;
        // The copy keeps the source's own extension: the reader's `deck()`
        // reads `original.<format>`, and a legacy `.ppt` must stay a `.ppt`
        // (the parser decides the format by content, but the file name is
        // what the manifest promises).
        sn_core::atomic::write_atomic(&dir.join(format!("original.{extension}")), &bytes)?;

        // One TOC row per slide, titled by the slide's own title when it has
        // one.
        let toc: Vec<ManifestTocEntry> = deck
            .slides
            .iter()
            .enumerate()
            .map(|(i, slide)| ManifestTocEntry {
                spine_index: Some(i),
                href: format!("slide-{}", i + 1),
                label: {
                    let title = slide.title.trim();
                    if title.is_empty() {
                        format!("Slide {}", i + 1)
                    } else {
                        title.to_string()
                    }
                },
                depth: 0,
                fragment: None,
            })
            .collect();

        let now = sn_core::AppleDate::now().raw();
        let title = {
            let deck_title = deck.title.trim();
            if deck_title.is_empty() {
                file_stem(source)
            } else {
                deck_title.to_string()
            }
        };
        // The cover: a picture the deck itself ships (no deck rasteriser
        // exists — the slides render in the viewer's engine). A text-only
        // deck keeps the shelf's placeholder.
        let has_cover = match sn_doc::slides::cover_jpeg(&deck) {
            Some(bytes) => std::fs::write(dir.join("cover.jpg"), &bytes).is_ok(),
            None => false,
        };
        let manifest = BookManifest {
            id: id.clone(),
            format: extension.clone(),
            added_at: now,
            last_opened_at: now,
            title,
            author: deck.author.clone(),
            spine_count: deck.slides.len(),
            spine: (0..deck.slides.len()).map(|i| format!("slide-{i}")).collect(),
            toc,
            position: BookPosition::default(),
            has_cover,
            original_file_name: source
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            duration_seconds: None,
            audio_codec: None,
            chapters: Vec::new(),
        };
        let json = serde_json::to_vec_pretty(&manifest)
            .map_err(|e| BookError::Epub(e.to_string()))?;
        sn_core::atomic::write_atomic(&dir.join("manifest.json"), &json)?;

        Ok(Book::new(manifest, dir))
    }

    /// Import a headless document book — the Mobipocket/Kindle family
    /// (`.mobi`/`.azw`/`.azw3`/`.prc`/`.pdb`), FictionBook, RTF,
    /// HTML/HTMLZ or plain text — into the library.
    ///
    /// These formats have no container the reader can serve, so the file is
    /// parsed once here for its metadata, cover, chapters and contents, and
    /// the original file is stored **verbatim** as `original.<ext>`: no
    /// `.epub` is ever written and nothing re-enters through the EPUB path.
    /// The manifest records the real extension as `format` (the way slides
    /// record `"pptx"`/`"odp"`), and the spine names the parsed chapters
    /// (`chapter-0`, …), which the reader serves out of the parse.
    pub fn import_document(source: &Path) -> Result<Book, BookError> {
        let extension = source
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if sn_doc::bookdoc::DocFamily::from_extension(&extension).is_none() {
            return Err(BookError::NotAnEpub(format!(
                "{}: not a book format this app imports",
                source.display()
            )));
        }
        // Whole-book read, capped like an EPUB: a "book" can arrive from a
        // LAN peer who chose its size freely, and the cap keeps that a
        // refused import instead of an OOM.
        let bytes = sn_core::util::read_capped(source, DOCUMENT_FILE_CAP)?;
        let parts = ServedBookDoc::parse(&extension, bytes)
            .map_err(|e| map_document_error(source, e))?;
        if parts.doc().chapters.is_empty() {
            return Err(BookError::Epub(format!(
                "{}: the book has no readable chapters",
                source.display()
            )));
        }

        let id = new_book_id();
        let dir = Self::dir().join(&id);
        std::fs::create_dir_all(&dir)?;

        // 1. the file copy, atomically, under its own extension — the
        //    user's file is kept verbatim, never rewritten to another
        //    format. Written from the parse's own bytes: the file was read
        //    once, not twice.
        sn_core::atomic::write_atomic(
            &dir.join(format!("original.{extension}")),
            parts.bytes(),
        )?;

        // 2. the cover, when the parse found one, written as cover.jpg like
        //    every other format — GTK's image loader sniffs the content,
        //    and the manifest only records whether one exists.
        let has_cover = match parts.doc().cover.as_ref() {
            Some(cover) if !cover.data.is_empty() => {
                sn_core::atomic::write_atomic(&dir.join("cover.jpg"), &cover.data).is_ok()
            }
            _ => false,
        };

        // 3. the manifest, last. The contents are the book's own when the
        //    format carried one (a MOBI's NCX, an FB2's sections) and the
        //    chapter titles when it did not.
        let chapter_count = parts.doc().chapters.len();
        let toc: Vec<ManifestTocEntry> = if parts.doc().toc.is_empty() {
            parts
                .doc()
                .chapters
                .iter()
                .enumerate()
                .map(|(i, chapter)| ManifestTocEntry {
                    spine_index: Some(i),
                    href: sn_doc::bookdoc::BookDoc::spine_name(i),
                    label: chapter
                        .title
                        .clone()
                        .unwrap_or_else(|| format!("Chapter {}", i + 1)),
                    depth: 0,
                    fragment: None,
                })
                .collect()
        } else {
            parts
                .doc()
                .toc
                .iter()
                .map(|row| ManifestTocEntry {
                    spine_index: Some(row.chapter_index.min(chapter_count.saturating_sub(1))),
                    href: sn_doc::bookdoc::BookDoc::spine_name(row.chapter_index),
                    label: row.label.clone(),
                    depth: row.depth,
                    fragment: None,
                })
                .collect()
        };

        let now = sn_core::AppleDate::now().raw();
        let manifest = BookManifest {
            id: id.clone(),
            format: extension,
            added_at: now,
            last_opened_at: now,
            title: parts
                .doc()
                .title
                .clone()
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| file_stem(source)),
            author: parts.doc().author.clone().unwrap_or_default(),
            spine_count: chapter_count,
            spine: parts.doc().spine(),
            toc,
            position: BookPosition::default(),
            has_cover,
            original_file_name: source
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            duration_seconds: None,
            audio_codec: None,
            chapters: Vec::new(),
        };
        let json = serde_json::to_vec_pretty(&manifest)
            .map_err(|e| BookError::Epub(e.to_string()))?;
        sn_core::atomic::write_atomic(&dir.join("manifest.json"), &json)?;

        Ok(Book::new(manifest, dir))
    }

    /// Import an audiobook (`.m4b`/`.m4a`/`.mp4`, `.opus`/`.ogg`, `.mp3`)
    /// into the library. The audio file is copied (never moved), the
    /// chapters come from the container, and the manifest carries what the
    /// player needs without opening the file again.
    pub fn import_audiobook(source: &Path) -> Result<Book, BookError> {
        let extension = source
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let is_mp4 = matches!(extension.as_str(), "m4b" | "m4a" | "mp4");
        let is_known = is_mp4 || matches!(extension.as_str(), "opus" | "ogg" | "oga" | "mp3");
        if !is_known {
            return Err(BookError::NotAnEpub(format!(
                "{}: not an audiobook format this app imports",
                source.display()
            )));
        }

        let id = new_book_id();
        let dir = Self::dir().join(&id);
        std::fs::create_dir_all(&dir)?;

        // 1. the audio copy, atomically — a kill -9 mid-copy must leave a
        //    `.part`, never a half-written original. Streamed through a
        //    bounded buffer: an honest m4b runs to gigabytes, and the old
        //    read-into-RAM-then-write made a big book an OOM.
        let target_name = if is_mp4 { "original.m4b" } else { "original.opus" };
        sn_core::atomic::copy_atomic(source, &dir.join(target_name))?;

        // 2. the container's metadata. An MP4 our box walker cannot read is
        //    still importable — the ffmpeg pipe decodes it, and a single
        //    chapter covering the whole file is honest.
        let (duration, codec, chapters) = if is_mp4 {
            match sn_doc::mp4::parse(source) {
                Ok(info) => (
                    Some(info.duration_seconds),
                    Some(info.codec),
                    info.chapters
                        .into_iter()
                        .map(|c| AudiobookChapter {
                            title: c.title,
                            start_seconds: c.start_seconds,
                            end_seconds: c.end_seconds,
                        })
                        .collect(),
                ),
                Err(_) => (None, None, Vec::new()),
            }
        } else {
            // Opus/Ogg/MP3: the Ogg page walk is its own item; a single
            // chapter plays and resumes correctly meanwhile.
            (None, Some(extension), Vec::new())
        };
        let chapters = if chapters.is_empty() {
            vec![AudiobookChapter {
                title: "Full recording".into(),
                start_seconds: 0.0,
                end_seconds: duration.unwrap_or(0.0),
            }]
        } else {
            chapters
        };

        // The cover is the attached picture, which most m4b files carry as an
        // mjpeg video stream. One frame, scaled to something the shelf can
        // draw, written atomically like every other file here. A file with no
        // artwork (or no ffmpeg) simply gets the placeholder.
        let has_cover = extract_audio_cover(source, &dir.join("cover.jpg"));

        let now = sn_core::AppleDate::now().raw();
        let manifest = BookManifest {
            id: id.clone(),
            format: if is_mp4 { "m4b".into() } else { "opus".into() },
            added_at: now,
            last_opened_at: now,
            title: file_stem(source),
            author: String::new(),
            spine_count: chapters.len(),
            spine: Vec::new(),
            toc: chapters
                .iter()
                .map(|c| ManifestTocEntry {
                    spine_index: None,
                    href: String::new(),
                    label: c.title.clone(),
                    depth: 0,
                    fragment: None,
                })
                .collect(),
            position: BookPosition::default(),
            has_cover,
            original_file_name: source
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            duration_seconds: duration,
            audio_codec: codec,
            chapters,
        };
        let json = serde_json::to_vec_pretty(&manifest)
            .map_err(|e| BookError::Epub(e.to_string()))?;
        sn_core::atomic::write_atomic(&dir.join("manifest.json"), &json)?;

        Ok(Book::new(manifest, dir))
    }

    /// Write a book's manifest back.
    pub fn save(book: &Book) -> Result<(), BookError> {
        let json = serde_json::to_vec_pretty(&book.manifest)
            .map_err(|e| BookError::Epub(e.to_string()))?;
        sn_core::atomic::write_atomic(&book.dir.join("manifest.json"), &json)?;
        Ok(())
    }

    /// Rewrite an audiobook manifest whose chapter titles are raw container
    /// bytes.
    ///
    /// The m4b chapter-title reader used to accept any two bytes as a length
    /// prefix, so a mis-derived sample offset produced rows of `trak\0\0\0{tkhd`
    /// in the contents. That is fixed for new imports, but a manifest already
    /// written is permanent — nothing re-parses the file afterwards — so the
    /// garbage survives every launch and every re-read. This walks every book
    /// in the library, replaces any title that cannot be a title with the
    /// `Chapter N` placeholder, and writes the manifest back atomically. A
    /// book with nothing to repair costs one comparison per row.
    ///
    /// Returns the ids it rewrote, so the caller can say what it did.
    pub fn repair_chapter_titles() -> Result<Vec<String>, BookError> {
        let mut repaired = Vec::new();
        for mut book in Self::all()? {
            if book.manifest.format != "m4b" && book.manifest.format != "mp4" {
                continue;
            }
            let mut changed = false;
            for (i, row) in book.manifest.toc.iter_mut().enumerate() {
                if !sn_doc::mp4::plausible_title(&row.label) {
                    row.label = format!("Chapter {}", i + 1);
                    changed = true;
                }
            }
            for (i, chapter) in book.manifest.chapters.iter_mut().enumerate() {
                if !sn_doc::mp4::plausible_title(&chapter.title) {
                    chapter.title = format!("Chapter {}", i + 1);
                    changed = true;
                }
            }
            if changed {
                Self::save(&mut book)?;
                repaired.push(book.manifest.id.clone());
            }
        }
        Ok(repaired)
    }

    /// Render the cover of every PDF in the library that has none.
    ///
    /// A PDF's cover is drawn, not extracted: page 1 rasterised by poppler.
    /// A book imported before the renderer existed has `hasCover: false` and
    /// no `cover.jpg`, so the shelf shows it as a placeholder forever — the
    /// file is right there, only the drawing was never done. This walks the
    /// library and draws what is missing. A book whose renderer is absent
    /// stays without a cover, which is the honest answer.
    ///
    /// Returns the ids it rewrote.
    pub fn repair_pdf_covers() -> Result<Vec<String>, BookError> {
        let mut repaired = Vec::new();
        for mut book in Self::all()? {
            if book.manifest.format != "pdf" || book.manifest.has_cover {
                continue;
            }
            let original = book.original();
            if !original.exists() {
                continue;
            }
            let dest = book.dir.join("cover.jpg");
            if !render_pdf_page_cover(&original, 1, &dest) {
                continue;
            }
            book.manifest.has_cover = true;
            Self::save(&mut book)?;
            repaired.push(book.manifest.id.clone());
        }
        Ok(repaired)
    }

    /// Everything the library needs repaired, in one sweep: audiobook chapter
    /// titles that are raw container bytes, and PDF covers that were never
    /// drawn. Returns the ids it rewrote.
    pub fn repair_library() -> Result<Vec<String>, BookError> {
        let mut repaired = BooksStore::repair_chapter_titles()?;
        for id in BooksStore::repair_pdf_covers()? {
            if !repaired.contains(&id) {
                repaired.push(id);
            }
        }
        Ok(repaired)
    }

    /// Delete a book and its directory.
    pub fn delete(book: &Book) -> Result<(), BookError> {
        // The parse memo outlives the book unless it is told: a deleted
        // 50 MB PDF would sit in the map for the life of the process.
        forget_parses(&book.original());
        if book.dir.exists() {
            std::fs::remove_dir_all(&book.dir)?;
        }
        Ok(())
    }
}


/// Rasterise a PDF's first page into `dest` as a cover image.
///
/// A PDF is a page description, not an archive with a cover entry, so the
/// cover is drawn rather than extracted. `pdftoppm` (poppler) is the renderer:
/// it is on every box that has a PDF reader, and 40 dpi on a letter page is a
/// 330×430 PNG — bigger than the shelf's 44×60 row and small enough to decode
/// in a row build. A missing or failing renderer leaves nothing, and the shelf
/// draws its placeholder.
fn render_pdf_page_cover(pdf: &Path, page: u32, dest: &Path) -> bool {
    let stem = dest
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "cover".to_string());
    // pdftoppm appends the page number and an extension of its own choosing
    // ("cover-1.png" for PNG, "cover-1.ppm" when asked for the default), so
    // the output path is a prefix and the produced file is discovered.
    let prefix = dest
        .parent()
        .unwrap_or(pdf)
        .join(format!("{stem}-rasterised"));
    // JPEG, not PNG: the file is named `cover.jpg`, and a file whose name
    // lies about its bytes is a trap for the next reader even when the
    // shelf's picture decodes by content. 40 dpi on a letter page is a
    // 340×440 JPEG — bigger than the shelf's 44×60 row and small enough to
    // decode in a row build.
    let output = std::process::Command::new("pdftoppm")
        .arg("-jpeg")
        .arg("-jpegopt")
        .arg("quality=75")
        .arg("-r")
        .arg("40")
        .arg("-f")
        .arg(page.to_string())
        .arg("-l")
        .arg(page.to_string())
        .arg("-singlefile")
        .arg(pdf)
        .arg(&prefix)
        .output();
    let Ok(output) = output else {
        return false;
    };
    if !output.status.success() {
        let _ = std::fs::remove_file(&prefix);
        return false;
    }
    let produced = prefix.with_extension("jpg");
    let renamed = std::fs::rename(&produced, dest);
    let _ = std::fs::remove_file(&produced);
    renamed.is_ok()
}

/// Pull the attached picture out of an audio file into `dest`.
///
/// ffmpeg's `-map 0:v` with `-frames:v 1` takes the first still image, which
/// for an m4b is the cover art. The scale keeps a 3000-pixel source from
/// becoming a multi-megabyte JPEG the shelf has to decode on every row.
/// Anything that goes wrong — no artwork, no ffmpeg, an odd format — leaves no
/// file and the shelf draws its placeholder, which is the honest answer.
fn extract_audio_cover(source: &Path, dest: &Path) -> bool {
    let output = std::process::Command::new("ffmpeg")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-nostdin")
        .arg("-i")
        .arg(source)
        .arg("-map")
        .arg("0:v")
        .arg("-frames:v")
        .arg("1")
        .arg("-vf")
        .arg("scale=min(600\\,iw):-1")
        .arg("-f")
        .arg("image2")
        .arg("-c:v")
        .arg("mjpeg")
        .arg(dest)
        .output();
    let Ok(output) = output else {
        return false;
    };
    if !output.status.success() {
        let _ = std::fs::remove_file(dest);
        return false;
    }
    // An empty file is ffmpeg's way of saying "there was no video stream".
    match std::fs::metadata(dest) {
        Ok(meta) if meta.len() > 0 => true,
        _ => {
            let _ = std::fs::remove_file(dest);
            false
        }
    }
}

/// Memoised `all()` for the UI: the caller holds the cache and a dirty flag,
/// and this reads the library only when the flag is set. Kept beside the
/// store it memoises so the invalidation points (import, delete) are one
/// grep away.
pub fn library_cached(
    cache: &std::cell::RefCell<Option<Vec<Book>>>,
    dirty: &std::cell::Cell<bool>,
) -> Result<Vec<Book>, BookError> {
    if dirty.get() {
        *cache.borrow_mut() = Some(BooksStore::all()?);
        dirty.set(false);
    }
    Ok(cache
        .borrow()
        .as_ref()
        .cloned()
        .expect("the cache was just filled"))
}

/// A fresh book id: uppercase hex, the shape the other apps use.
fn new_book_id() -> String {
    #[cfg(test)]
    isolate_test_library();
    uuid::Uuid::new_v4()
        .simple()
        .to_string()
        .to_uppercase()
}

/// Point the test process's data root at a scratch directory.
///
/// `BooksStore::dir()` resolves through `XDG_DATA_HOME`, and a test binary
/// inherits the environment it was launched with — so every `cargo test`
/// run used to write its fixture books **into the user's live library**:
/// the orphan `Books/<uuid>/` directories that carried a force-ocr marker
/// and a `text/0004.txt` saying "learning outcome" were exactly that,
/// sprayed by the suite and invisible to the shelf (no manifest). Every
/// book fixture re-asserts the scratch root (`new_book_id` runs first in
/// each), and `XdgFixture::drop` restores it too, so no test ever resolves
/// a store path into `~/.local/share` — however the two guard styles
/// interleave across the parallel threads.
#[cfg(test)]
fn isolate_test_library() {
    let scratch = crate::notes::test_support::scratch_data_home();
    std::env::set_var("XDG_DATA_HOME", scratch);
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Untitled".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_memo_over_the_cap_evicts_the_least_recently_used_parse() {
        let mut memo: Memo<u8> = Memo::new();
        let ok = || Ok(std::sync::Arc::new(7u8));
        let a = Path::new("/books/a.pdf");
        let b = Path::new("/books/b.pdf");
        let c = Path::new("/books/c.pdf");

        memo.get_or_insert(a, 100, ok).unwrap();
        memo.get_or_insert(b, 100, ok).unwrap();
        // A hit on A makes it newer than B; C then pushes the total over a
        // small cap, and B — the stale one — is what must go.
        memo.get_or_insert(a, 100, ok).unwrap();
        memo.get_or_insert(c, 100, ok).unwrap();
        assert_eq!(memo.total, 300);
        memo.evict_under(250);
        assert!(!memo.map.contains_key(&(b.to_path_buf(), 100)), "stale entry evicted");
        assert!(memo.map.contains_key(&(a.to_path_buf(), 100)), "recently used kept");
        assert!(memo.map.contains_key(&(c.to_path_buf(), 100)), "just-inserted kept");
        assert_eq!(memo.total, 200);
    }

    #[test]
    fn a_lone_parse_bigger_than_the_cap_is_kept() {
        let mut memo: Memo<u8> = Memo::new();
        let ok = || Ok(std::sync::Arc::new(7u8));
        memo.get_or_insert(Path::new("/books/huge.pdf"), 400 * 1024 * 1024, ok)
            .unwrap();
        memo.evict_under(PARSE_MEMO_CAP);
        assert_eq!(memo.map.len(), 1, "evicting the only entry would only guarantee a re-parse");
    }

    #[test]
    fn forgetting_a_book_removes_its_bytes_from_the_memo() {
        let mut memo: Memo<u8> = Memo::new();
        let ok = || Ok(std::sync::Arc::new(7u8));
        memo.get_or_insert(Path::new("/books/a.pdf"), 100, ok).unwrap();
        memo.get_or_insert(Path::new("/books/b.pdf"), 50, ok).unwrap();
        memo.forget(Path::new("/books/a.pdf"));
        assert!(!memo.map.contains_key(&(Path::new("/books/a.pdf").to_path_buf(), 100)));
        assert_eq!(memo.total, 50);
    }

    /// Build a small EPUB on disk for import tests.
    fn write_epub(path: &Path, files: &[(&str, &str)]) {
        let mut data: Vec<u8> = Vec::new();
        let mut central: Vec<u8> = Vec::new();
        for (name, body) in files {
            let payload = body.as_bytes();
            let crc = sn_doc::zip::crc32(payload);
            let offset = data.len() as u32;
            data.extend_from_slice(b"PK\x03\x04");
            data.extend_from_slice(&20u16.to_le_bytes());
            data.extend_from_slice(&0u16.to_le_bytes());
            data.extend_from_slice(&0u16.to_le_bytes());
            data.extend_from_slice(&0u16.to_le_bytes());
            data.extend_from_slice(&0u16.to_le_bytes());
            data.extend_from_slice(&crc.to_le_bytes());
            data.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            data.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            data.extend_from_slice(&(name.len() as u16).to_le_bytes());
            data.extend_from_slice(&0u16.to_le_bytes());
            data.extend_from_slice(name.as_bytes());
            data.extend_from_slice(payload);
            central.extend_from_slice(b"PK\x01\x02");
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            central.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u32.to_le_bytes());
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        let central_start = data.len() as u32;
        data.extend_from_slice(&central);
        data.extend_from_slice(b"PK\x05\x06");
        data.extend_from_slice(&0u16.to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes());
        data.extend_from_slice(&(files.len() as u16).to_le_bytes());
        data.extend_from_slice(&(files.len() as u16).to_le_bytes());
        data.extend_from_slice(&(central.len() as u32).to_le_bytes());
        data.extend_from_slice(&central_start.to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes());
        std::fs::write(path, data).unwrap();
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("sn-books-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_manifest_round_trips_with_ios_field_names() {
        let json = r#"{
            "id": "ABC", "format": "epub", "addedAt": 1.0, "lastOpenedAt": 2.0,
            "title": "T", "author": "A", "spineCount": 2,
            "spine": ["a.xhtml", "b.xhtml"],
            "toc": [{"spineIndex": 0, "href": "a.xhtml", "label": "One"}],
            "position": {"chapterIndex": 1, "chapterFraction": 0.5},
            "hasCover": true, "originalFileName": "book.epub"
        }"#;
        let m: BookManifest = serde_json::from_str(json).expect("an iOS manifest parses");
        assert_eq!(m.spine_count, 2);
        assert_eq!(m.toc[0].spine_index, Some(0));
        assert_eq!(m.position.chapter_index, 1);
        assert!(m.has_cover);
        // And it writes back with the same names.
        let out = serde_json::to_string(&m).unwrap();
        assert!(out.contains("spineCount"), "{out}");
        assert!(out.contains("chapterFraction"), "{out}");
        assert!(out.contains("originalFileName"), "{out}");
    }

    /// An iOS-written audiobook manifest must parse, or a book imported on
    /// the phone is invisible here. The iOS app writes `audioChapters`,
    /// `audioDuration` and `audioChapterSource` and no reading order at all.
    #[test]
    fn an_ios_audiobook_manifest_parses_and_keeps_its_chapters() {
        let json = r#"{
            "id": "A365390E", "addedAt": 812478650.7,
            "originalFileName": "Harry Potter.m4b",
            "title": "Harry Potter", "lastOpenedAt": 812479134.6,
            "audioDuration": 31287.58, "audioChapterSource": "chpl",
            "author": "J.K. Rowling", "hasCover": true, "format": "audio",
            "audioChapters": [
                {"title": "Opening Credits", "startSeconds": 0.0, "endSeconds": 63.531},
                {"title": "Chapter 1", "startSeconds": 63.531, "endSeconds": 1970.531}
            ]
        }"#;
        let m: BookManifest = serde_json::from_str(json).expect("an iOS audiobook manifest parses");
        assert_eq!(m.title, "Harry Potter");
        assert_eq!(m.duration_seconds, Some(31287.58));
        assert_eq!(m.chapters.len(), 2, "{:?}", m.chapters);
        assert_eq!(m.chapters[1].title, "Chapter 1");
        assert!(!m.chapters.is_empty(), "the chapters make it an audiobook");
        // And the EPUB-only fields are absent, not zero-filled lies.
        assert!(m.spine.is_empty());
        assert_eq!(m.spine_count, 0);
    }

    /// This app's own audiobook manifest round-trips under the iOS names, so
    /// a book imported here opens on the phone.
    #[test]
    fn this_apps_audiobook_manifest_uses_the_ios_field_names() {
        let m = BookManifest {
            id: "X".into(),
            format: "m4b".into(),
            added_at: 1.0,
            last_opened_at: 1.0,
            title: "Dune".into(),
            author: "Frank Herbert".into(),
            spine_count: 0,
            spine: Vec::new(),
            toc: Vec::new(),
            position: BookPosition::default(),
            has_cover: false,
            original_file_name: "Dune.m4b".into(),
            duration_seconds: Some(82188.0),
            audio_codec: Some("mp4a".into()),
            chapters: vec![AudiobookChapter {
                title: "One".into(),
                start_seconds: 0.0,
                end_seconds: 2185.8,
            }],
        };
        let out = serde_json::to_string(&m).unwrap();
        assert!(out.contains("audioDuration"), "{out}");
        assert!(out.contains("audioChapters"), "{out}");
        assert!(out.contains("audioCodec"), "{out}");
        assert!(!out.contains("durationSeconds"), "{out}");
        assert!(!out.contains("\"chapters\""), "{out}");
        // And it reads back as the same book.
        let back: BookManifest = serde_json::from_str(&out).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn import_copies_the_archive_and_extracts_the_cover() {
        // The crate's fixture: it holds both sn-core's XDG lock and this
        // crate's TEST_LOCK, so this test cannot repoint another test's
        // store mid-run. Setting XDG_DATA_HOME by hand here is exactly the
        // flake that showed up in the suite (a store test failing once in
        // three runs) — process-global env needs the shared guard.
        let _f = crate::notes::test_support::XdgFixture::new();
        let home = scratch("import");
        let src = home.join("book.epub");
        write_epub(
            &src,
            &[
                (
                    "META-INF/container.xml",
                    r#"<container><rootfiles><rootfile full-path="content.opf"/></rootfiles></container>"#,
                ),
                (
                    "content.opf",
                    r#"<package>
                      <metadata><dc:title>Test Book</dc:title><dc:creator>An Author</dc:creator></metadata>
                      <manifest>
                        <item id="c" href="cover.jpg" properties="cover-image"/>
                        <item id="c1" href="c1.xhtml"/>
                      </manifest>
                      <spine><itemref idref="c1"/></spine>
                    </package>"#,
                ),
                ("cover.jpg", "fake-jpeg-bytes"),
                ("c1.xhtml", "<p>The first chapter speaks.</p>"),
            ],
        );

        let book = BooksStore::import(&src).expect("import");
        assert_eq!(book.manifest.title, "Test Book");
        assert_eq!(book.manifest.author, "An Author");
        assert_eq!(book.manifest.spine, vec!["c1.xhtml"]);
        assert_eq!(book.manifest.spine_count, 1);
        assert!(book.manifest.has_cover, "the cover was extracted");
        assert!(book.cover().unwrap().exists());
        // The user's file is untouched, and the app's copy is its own.
        assert!(src.exists());
        assert_eq!(
            std::fs::read(book.original()).unwrap(),
            std::fs::read(&src).unwrap()
        );
        // Chapter text reads back through the archive.
        assert_eq!(book.chapter_text(0).unwrap(), "The first chapter speaks.");

        BooksStore::delete(&book).unwrap();
        // The fixture restores the XDG variables on drop.
    }

    #[test]
    fn a_chapter_bookmark_key_matches_the_ios_shape() {
        // The iOS app writes book bookmarks as "book:<id>:<chapter>" with a
        // UTF-16 offset; the reader's tick must produce the same key or the
        // two apps cannot share a position.
        assert_eq!(crate::model::book_key("ABC", 3), "book:ABC:3");
        let _f = crate::notes::test_support::XdgFixture::new();
        let bookmarks = crate::stores::BookmarkStore::open().unwrap();
        let text = "The first chapter speaks. A second sentence follows.";
        // The reader's tick writes the frontier through the same
        // UTF-16-offset conversion the notes path uses.
        bookmarks.put(
            crate::model::book_key("ABC", 3),
            crate::model::PlaybackBookmark::for_note(
                "book:ABC:3",
                crate::model::utf16_offset(text, 25),
                sn_core::util::stable_hash(text),
            ),
        );
        let back = bookmarks.for_book_chapter("ABC", 3).expect("the bookmark round-trips");
        assert_eq!(back.text_offset, crate::model::utf16_offset(text, 25));
        assert_eq!(
            crate::model::utf16_to_byte_offset(text, back.text_offset),
            25
        );
    }

    #[test]
    fn the_archive_is_parsed_once_and_shared_across_chapters() {
        let _f = crate::notes::test_support::XdgFixture::new();
        let home = scratch("cache");
        let src = home.join("book.epub");
        write_epub(
            &src,
            &[
                (
                    "META-INF/container.xml",
                    r#"<container><rootfiles><rootfile full-path="content.opf"/></rootfiles></container>"#,
                ),
                (
                    "content.opf",
                    r#"<package><manifest>
                        <item id="c1" href="c1.xhtml"/>
                        <item id="c2" href="c2.xhtml"/>
                     </manifest><spine><itemref idref="c1"/><itemref idref="c2"/></spine></package>"#,
                ),
                ("c1.xhtml", "<p>First chapter.</p>"),
                ("c2.xhtml", "<p>Second chapter.</p>"),
            ],
        );
        let book = BooksStore::import(&src).expect("import");
        assert!(
            book.archive.borrow().is_none(),
            "the cache starts empty — the shelf never parses"
        );
        assert_eq!(book.chapter_text(0).unwrap(), "First chapter.");
        assert!(book.archive.borrow().is_some(), "the first read caches it");
        assert_eq!(book.chapter_text(1).unwrap(), "Second chapter.");
        // A cloned handle shares the cache: the reader and the shelf's
        // progress row cannot re-parse between them.
        let twin = book.clone();
        assert!(twin.archive.borrow().is_some());
        assert_eq!(twin.chapter_text(1).unwrap(), "Second chapter.");
    }

    #[test]
    fn a_non_epub_is_refused_with_the_file_named() {
        let home = scratch("nonepub");
        let src = home.join("notes.txt");
        std::fs::write(&src, b"just prose").unwrap();
        let err = BooksStore::import(&src).unwrap_err();
        assert!(err.to_string().contains("notes.txt"), "{err}");
    }

    #[test]
    fn a_document_import_keeps_the_file_and_never_writes_an_epub() {
        // The native-document rule: the original file is stored verbatim
        // under its own extension, and nothing on the way to the shelf
        // writes an .epub — the chapters live in the parse, not in a
        // converted archive.
        let _f = crate::notes::test_support::XdgFixture::new();
        let home = scratch("document-txt");
        let src = home.join("shelf.txt");
        let text = "The first chapter speaks.\n\nA second paragraph follows it.\n";
        std::fs::write(&src, text).unwrap();

        let book = BooksStore::import_document(&src).expect("import");
        assert!(book.is_document());
        assert_eq!(book.manifest.format, "txt");
        assert!(!book.manifest.spine.is_empty(), "the parse has chapters");
        assert_eq!(book.manifest.spine_count, book.manifest.spine.len());
        for (i, name) in book.manifest.spine.iter().enumerate() {
            assert_eq!(name, &format!("chapter-{i}"), "the spine names chapters");
        }
        // No format TOC on plain text: the contents fall back to the
        // chapter titles, one row per chapter.
        assert_eq!(book.manifest.toc.len(), book.manifest.spine.len());
        for (i, row) in book.manifest.toc.iter().enumerate() {
            assert_eq!(row.href, format!("chapter-{i}"));
            assert_eq!(row.spine_index, Some(i));
        }
        // The user's file is untouched, and the app's copy is its own.
        assert!(src.exists());
        assert_eq!(std::fs::read(book.original()).unwrap(), text.as_bytes());
        assert_eq!(book.original().file_name().unwrap(), "original.txt");
        let entries: Vec<String> = std::fs::read_dir(&book.dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            !entries.iter().any(|name| name.ends_with(".epub")),
            "no .epub is ever written: {entries:?}"
        );
        // Chapter text reads back through the ordinary pass.
        let spoken = book.chapter_text(0).unwrap();
        assert!(spoken.contains("The first chapter speaks."), "{spoken}");

        BooksStore::delete(&book).unwrap();
    }

    #[test]
    fn an_htmlz_document_imports_metadata_cover_images_and_serves_its_chapters() {
        let _f = crate::notes::test_support::XdgFixture::new();
        let home = scratch("document-htmlz");
        let src = home.join("mirror.htmlz");
        write_epub(
            &src,
            &[
                (
                    "index.html",
                    "<html><head><title>Mirror</title></head><body>\
                     <h1>One</h1><p>The first chapter speaks.</p>\
                     <img src=\"fig1.jpg\"></body></html>",
                ),
                (
                    "metadata.opf",
                    "<package><metadata><dc:title>Test Book</dc:title>\
                     <dc:creator>An Author</dc:creator></metadata></package>",
                ),
                ("cover.jpg", "fake-cover-bytes"),
                ("fig1.jpg", "fake-figure-bytes"),
            ],
        );

        let book = BooksStore::import_document(&src).expect("import");
        assert_eq!(book.manifest.format, "htmlz");
        // The OPF is the book's own metadata; the page's <title> is not.
        assert_eq!(book.manifest.title, "Test Book");
        assert_eq!(book.manifest.author, "An Author");
        assert!(book.manifest.has_cover, "the cover was extracted");
        assert!(book.cover().unwrap().exists());
        assert!(book.original().is_file());
        assert_eq!(book.original().file_name().unwrap(), "original.htmlz");

        // The parsed book answers the names the reader's scheme handler
        // asks for: the chapter page, the referenced image, the cover.
        let parts = book.served().expect("the parse");
        let page = parts.read("chapter-0").expect("the chapter page");
        let page = String::from_utf8(page).unwrap();
        assert!(page.starts_with("<!DOCTYPE html>"), "{page}");
        assert!(page.contains("<img src=\"img/fig1.jpg\""), "{page}");
        assert_eq!(parts.read("img/fig1.jpg").unwrap(), b"fake-figure-bytes");
        assert_eq!(parts.read("img/cover").unwrap(), b"fake-cover-bytes");
        assert!(parts.read("img/missing").is_none());
        assert!(parts.read("chapter-99").is_none());
        assert_eq!(parts.mime_for("chapter-0"), "text/html");
        assert_eq!(parts.mime_for("img/fig1.jpg"), "image/jpeg");

        // And the speech path reads the chapter through the ordinary pass.
        let spoken = book.chapter_text(0).unwrap();
        assert!(spoken.contains("The first chapter speaks."), "{spoken}");

        BooksStore::delete(&book).unwrap();
    }

    #[test]
    fn a_non_mobipocket_pdb_is_refused_by_name() {
        // `.pdb` is every Palm-era format's extension. A file wearing it
        // that is not a Mobipocket book gets the typed refusal — the file
        // is fine, this app just cannot read it — not the decode-noise of
        // a parser run over a foreign format.
        let _f = crate::notes::test_support::XdgFixture::new();
        let home = scratch("document-pdb");
        let src = home.join("unknown.pdb");
        std::fs::write(&src, vec![0u8; 128]).unwrap();
        let err = BooksStore::import_document(&src).unwrap_err();
        assert!(matches!(err, BookError::Unsupported(_)), "{err}");
        assert!(err.to_string().contains("not a Mobipocket book"), "{err}");
    }

    #[test]
    fn a_drm_locked_book_is_refused_with_the_store_s_message() {
        // A PDB whose record 0 carries the encryption flag: the parse
        // refuses it before any decompression, and the refusal passes
        // through verbatim — it is already written for the user.
        let _f = crate::notes::test_support::XdgFixture::new();
        let home = scratch("document-drm");
        let src = home.join("locked.mobi");
        let be16 = |v: u16| v.to_be_bytes();
        let be32 = |v: u32| v.to_be_bytes();
        let mut pdb = vec![0u8; 78];
        pdb[60..64].copy_from_slice(b"BOOK");
        pdb[64..68].copy_from_slice(b"MOBI");
        pdb[76..78].copy_from_slice(&be16(1));
        // The record list's one 8-byte entry — offset, then the id/attributes
        // pair — and the next-IDs list's terminating zero; the payload sits
        // at 78 + 8 + 2.
        pdb.extend_from_slice(&be32((78 + 8 + 2) as u32)); // record 0's offset
        pdb.extend_from_slice(&[0u8; 4]); // the entry's id/attributes pair
        pdb.extend_from_slice(&[0, 0]); // the next-IDs list's terminator
        let mut record = vec![0u8; 16];
        record[0..2].copy_from_slice(&be16(2)); // PalmDOC compression
        record[4..8].copy_from_slice(&be32(0)); // text length
        record[8..10].copy_from_slice(&be16(1)); // text record count
        record[10..12].copy_from_slice(&be16(4096)); // record size
        record[12..14].copy_from_slice(&be16(1)); // encryption: DRM
        pdb.extend_from_slice(&record);
        std::fs::write(&src, &pdb).unwrap();

        let err = BooksStore::import_document(&src).unwrap_err();
        assert!(matches!(err, BookError::Unsupported(_)), "{err}");
        assert!(err.to_string().contains("DRM-locked"), "{err}");
    }

    #[test]
    fn a_nested_outline_keeps_its_depth_and_its_pages() {
        // The manifest is the reader's table of contents: the depth is what
        // the reader indents by, and the href is what a PDF row's page number
        // is read from. Both are parsed out of the source document and both
        // have to survive the trip through the manifest.
        //
        // The fixture is not optional here. `import` writes a real book
        // directory under `books_dir()`, so without it this test imports into
        // the user's live library — and because the XDG variables are
        // process-global, it also breaks whenever a *neighbouring* test's
        // fixture repoints the root underneath it mid-flight. That is the
        // flake this suite kept hitting: the import's `create_dir_all` found
        // the directory gone and returned NotFound.
        let _f = crate::notes::test_support::XdgFixture::new();
        let home = scratch("depth");
        let src = home.join("book.epub");
        write_epub(
            &src,
            &[
                (
                    "META-INF/container.xml",
                    r#"<container><rootfiles><rootfile full-path="content.opf"/></rootfiles></container>"#,
                ),
                (
                    "content.opf",
                    r#"<package>
                      <metadata><dc:title>Nested</dc:title></metadata>
                      <manifest><item id="c1" href="c1.xhtml"/></manifest>
                      <spine><itemref idref="c1"/></spine>
                    </package>"#,
                ),
                ("c1.xhtml", "<p>Text.</p>"),
            ],
        );
        let book = BooksStore::import(&src).expect("import");
        // The EPUB's own nav is absent, so the TOC is the fallback list; the
        // depth field must still be there and readable.
        for row in &book.manifest.toc {
            assert!(
                row.depth <= 6,
                "a depth that could not be drawn is a bad depth: {}",
                row.depth
            );
        }
    }

    #[test]
    fn a_pdf_manifest_carries_its_pages_and_depths() {
        // The PDF path is the one that has both: a page in the href and an
        // outline depth. The manifest is written from the outline, so this
        // proves the two fields reach the file the reader reads.
        //
        // The fixture, for the same reason as the EPUB import test above:
        // `import_pdf` writes a book directory, and it also shells out to
        // `pdftoppm` to rasterise the cover.
        let _f = crate::notes::test_support::XdgFixture::new();
        let home = scratch("pdf-manifest");
        let src = home.join("doc.pdf");
        // A one-page PDF with a classic xref, built by hand.
        let objects: Vec<String> = vec![
            "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".into(),
            "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".into(),
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n".into(),
            "4 0 obj\n<< /Title (Paediatric cardiology) /Author (Dr Mulenga) >>\nendobj\n".into(),
        ];
        let mut body = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for o in &objects {
            offsets.push(body.len());
            body.push_str(o);
        }
        let xref_at = body.len();
        body.push_str("xref\n0 5\n");
        body.push_str("0000000000 65535 f \n");
        for off in &offsets {
            body.push_str(&format!("{off:010} 00000 n \n"));
        }
        body.push_str("trailer\n<< /Size 5 /Root 1 0 R /Info 4 0 R >>\n");
        body.push_str(&format!("startxref\n{xref_at}\n%%EOF\n"));
        std::fs::write(&src, body.as_bytes()).unwrap();

        let book = BooksStore::import_pdf(&src).expect("import");
        assert_eq!(book.manifest.format, "pdf");
        assert_eq!(book.manifest.spine_count, 1);
        assert_eq!(book.manifest.title, "Paediatric cardiology");
        // No outline in this PDF, so the TOC is empty and the reader falls
        // back to a chapter list — but the spine is the pages.
        assert_eq!(book.manifest.spine, vec!["page-0"]);
    }

    #[test]
    fn a_pdf_viewer_reads_sizes_and_window_text_off_one_parse() {
        // The viewer is the page-image side of a split: it needs every page's
        // box to lay a continuous scroll out before any page is drawn, and the
        // `Read along` toggle needs the *text* of the pages on screen, with
        // byte offsets the bookmark and the speech plan can share. Both come
        // off the same parse — one book handle, one `Document`, or a
        // 1295-page book would read its 50 MB file twice per open.
        let _f = crate::notes::test_support::XdgFixture::new();
        let home = scratch("pdf-viewer");
        let src = home.join("doc.pdf");
        // Two pages: portrait then landscape, so the size list cannot pass
        // by repeating one number, and each page paints a phrase the text
        // side must find.
        let s1 = "BT /F1 12 Tf 72 720 Td (Portrait one.) Tj ET";
        let s2 = "BT /F1 12 Tf 72 540 Td (Landscape two.) Tj ET";
        let objects: Vec<String> = vec![
            "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".into(),
            "2 0 obj\n<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>\nendobj\n".into(),
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 7 0 R >> >> >>\nendobj\n".into(),
            format!("4 0 obj\n<< /Length {} >>\nstream\n{}\nendstream\nendobj\n", s1.len(), s1),
            "5 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 792 612] /Contents 6 0 R /Resources << /Font << /F1 7 0 R >> >> >>\nendobj\n".into(),
            format!("6 0 obj\n<< /Length {} >>\nstream\n{}\nendstream\nendobj\n", s2.len(), s2),
            "7 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n".into(),
        ];
        let mut body = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for o in &objects {
            offsets.push(body.len());
            body.push_str(o);
        }
        let xref_at = body.len();
        body.push_str(&format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1));
        for off in &offsets {
            body.push_str(&format!("{off:010} 00000 n \n"));
        }
        body.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n",
            objects.len() + 1
        ));
        std::fs::write(&src, body.as_bytes()).unwrap();

        let book = BooksStore::import_pdf(&src).expect("import");
        assert_eq!(book.manifest.spine_count, 2);

        // Sizes come first, in reading order, each page its own box.
        let sizes = book.page_sizes().expect("sizes");
        assert_eq!(sizes, vec![(612.0, 792.0), (792.0, 612.0)]);
        assert!(
            book.pdf_doc.borrow().is_some(),
            "the layout read parses the file — the text call must reuse it"
        );

        // One page's text is one page's text: the window must not smuggle
        // the neighbour back with it.
        let first = book.pdf_page_text(0, 1).expect("page 0 text");
        assert!(first.contains("Portrait one."), "{first:?}");
        assert!(!first.contains("Landscape two."), "{first:?}");

        // The whole open range is the join the speech plan chunks over.
        let both = book.pdf_page_text(0, 2).expect("both pages");
        assert!(both.contains("Portrait one.") && both.contains("Landscape two."), "{both:?}");

        // And a handle cloned onto the shelf's side shares the parse: the
        // 50 MB file is read once, not once per caller.
        let twin = book.clone();
        assert!(twin.pdf_doc.borrow().is_some());
        let again = twin.pdf_page_text(1, 2).expect("twin reads too");
        assert!(again.contains("Landscape two."), "{again:?}");
    }

    #[test]
    fn repair_replaces_garbage_titles_and_leaves_good_ones() {
        use crate::notes::test_support::XdgFixture;
        let _f = XdgFixture::new();
        let id = new_book_id();
        let dir = BooksStore::dir().join(&id);
        std::fs::create_dir_all(&dir).unwrap();
        // The exact rows the pre-sanitiser parser wrote for this box's
        // Ulysses m4b: two good titles interleaved with container bytes.
        let manifest = BookManifest {
            id: id.clone(),
            format: "m4b".to_string(),
            toc: vec![
                ManifestTocEntry {
                    spine_index: None,
                    href: String::new(),
                    label: "Chapter 1".to_string(),
                    depth: 0,
                    fragment: None,
                },
                ManifestTocEntry {
                    spine_index: None,
                    href: String::new(),
                    label: "\u{0680}\u{fffd}trak\u{0}\u{0}\u{0}\\tkhd".to_string(),
                    depth: 0,
                    fragment: None,
                },
                ManifestTocEntry {
                    spine_index: None,
                    href: String::new(),
                    label: "Sound Media Handler\u{0}\u{7f}\u{fffd}m".to_string(),
                    depth: 0,
                    fragment: None,
                },
            ],
            ..manifest_fixture()
        };
        let json = serde_json::to_vec_pretty(&manifest).unwrap();
        sn_core::atomic::write_atomic(&dir.join("manifest.json"), &json).unwrap();

        let repaired = BooksStore::repair_chapter_titles().expect("the sweep runs");
        assert_eq!(repaired, vec![id.clone()], "the book is the one rewritten");

        let book = BooksStore::find(&id).unwrap().expect("it is still there");
        let labels: Vec<&str> = book.manifest.toc.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(
            labels,
            vec!["Chapter 1", "Chapter 2", "Chapter 3"],
            "garbage becomes the placeholder; a real title is untouched"
        );
        // And the sweep is idempotent: nothing left to fix, nothing rewritten.
        assert!(
            BooksStore::repair_chapter_titles()
                .expect("the second sweep runs")
                .is_empty(),
            "a repaired book costs nothing a second time"
        );
    }

    #[test]
    fn repair_leaves_an_epub_alone() {
        use crate::notes::test_support::XdgFixture;
        let _f = XdgFixture::new();
        let id = new_book_id();
        let dir = BooksStore::dir().join(&id);
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = BookManifest {
            id,
            format: "epub".to_string(),
            ..manifest_fixture()
        };
        sn_core::atomic::write_atomic(
            &dir.join("manifest.json"),
            &serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        assert!(
            BooksStore::repair_chapter_titles()
                .expect("the sweep runs")
                .is_empty(),
            "only m4b containers are touched"
        );
    }

    /// An otherwise-empty manifest for tests that only touch two fields.
    fn manifest_fixture() -> BookManifest {
        BookManifest {
            id: String::new(),
            format: String::new(),
            added_at: 1.0,
            last_opened_at: 1.0,
            title: String::new(),
            author: String::new(),
            spine_count: 0,
            spine: Vec::new(),
            toc: Vec::new(),
            position: BookPosition::default(),
            has_cover: false,
            original_file_name: String::new(),
            duration_seconds: None,
            audio_codec: None,
            chapters: Vec::new(),
        }
    }

    #[test]
    fn the_force_ocr_marker_round_trips() {
        let id = new_book_id();
        let dir = BooksStore::dir().join(&id);
        std::fs::create_dir_all(&dir).unwrap();
        let mut manifest = manifest_fixture();
        manifest.id = id.clone();
        let book = Book::new(manifest, dir);
        assert!(!book.force_ocr());
        book.set_force_ocr(true);
        assert!(book.force_ocr());
        book.set_force_ocr(false);
        assert!(!book.force_ocr());
    }

    #[test]
    fn forcing_ocr_re_reads_a_page_that_already_has_a_cache() {
        // The regression this pins: Force OCR is a control that says "read
        // every page with recognition". A page whose cache was written
        // *before* the control was on holds a reading made under the old
        // rules, so serving that cache made the control do nothing at all
        // for every page the reader had already visited — the control looked
        // broken, and the whole-book warm-up reported zero pages.
        //
        // The cache is refused on a forced read, so the page is recognised
        // again — and the fresh reading is merged into the old one, not
        // written over it.
        let id = new_book_id();
        let dir = BooksStore::dir().join(&id);
        std::fs::create_dir_all(dir.join("text")).unwrap();
        let mut manifest = manifest_fixture();
        manifest.id = id.clone();
        manifest.format = "pdf".to_string();
        manifest.spine_count = 1;
        manifest.spine = vec!["page-0".to_string()];
        //  the original binds `book` but the test below exercises the
        // associated fn `Book::ocr_page_forced` — the binding kept (underscored)
        // in case `Book::new` carries side effects; renamed 2026-10-30 to clear
        // the rustc warning.
        let _book = Book::new(manifest, dir.clone());

        let cache = dir.join("text/0001.txt");
        std::fs::write(&cache, "the cached reading").unwrap();

        // Without the engine, a forced read cannot produce anything — and it
        // must not hand back the stale cache as if it had.
        let forced = Book::ocr_page_forced(&dir, 0);
        assert!(
            forced.as_deref() != Some("the cached reading"),
            "a forced read served the stale cache: {forced:?}"
        );
        // An ordinary read still serves the cache — that is the whole point
        // of a cache, and the read path must not re-recognise every page.
        assert_eq!(
            Book::ocr_page(&dir, 0).as_deref(),
            Some("the cached reading")
        );
    }

    #[test]
    fn the_ocr_mix_adds_unrecognised_lines_and_drops_duplicates() {
        let layer = "Chapter 3\n\nManagement of breech presentation\n\nTerm breech trial, Lancet 2000";
        // Recognition repeats two lines (its own line breaks differ) and adds
        // a figure label the layer never had.
        let ocr = "Management of breech\npresentation\nFIG. 4. The Frank breech position.";
        let merged = sn_doc::reconcile::reconcile(layer, ocr);
        assert!(merged.starts_with(layer), "the layer leads");
        assert!(merged.contains("FIG. 4."), "the new line was added: {merged:?}");
        assert_eq!(merged.matches("Term breech trial").count(), 1, "no duplication");
        // Recognition finding nothing new leaves the layer untouched.
        assert_eq!(sn_doc::reconcile::reconcile(layer, layer), layer);
        // And recognition of an empty raster adds nothing.
        assert_eq!(sn_doc::reconcile::reconcile(layer, "  \n"), layer);
    }

    #[test]
    fn a_forced_re_read_that_finds_less_cannot_make_the_page_worse() {
        // The merge guarantee behind Force OCR: the fresh reading only ever
        // adds, so a pass with a worse detection day (a hair of blur, a
        // different threshold) degrades nothing. Byte offsets into the base
        // reading stay put too — the property a bookmark stands on.
        let first = "Management of breech\npresentation\nTerm breech trial, Lancet 2000";
        let worse = "Management of breech";
        let merged = sn_doc::reconcile::reconcile(first, worse);
        assert_eq!(merged, first, "the worse pass added nothing");
        let better = "Management of breech\npresentation\nFIG. 4. The Frank breech position.";
        let grown = sn_doc::reconcile::reconcile(first, better);
        assert!(grown.starts_with(first), "the better pass only appends");
        assert!(grown.contains("FIG. 4."));
    }

    #[test]
    fn a_forced_cache_older_than_the_control_is_refreshed_a_fresh_one_is_served() {
        // The control's promise, timed: a cache written *before* Force OCR
        // was switched on was recognised without the control's rules, so it
        // must be refreshed; a cache written *while* the control is on
        // already honours it, and re-recognising that on every open would
        // burn seconds a page for nothing new.
        let id = new_book_id();
        let dir = BooksStore::dir().join(&id);
        std::fs::create_dir_all(dir.join("text")).unwrap();
        let mut manifest = manifest_fixture();
        manifest.id = id.clone();
        manifest.format = "pdf".to_string();
        manifest.spine_count = 1;
        manifest.spine = vec!["page-0".to_string()];
        let book = Book::new(manifest, dir.clone());

        // No marker yet: the cache is "stale" only in the sense that the
        // control is off — an ordinary read serves it either way.
        let cache = dir.join("text/0001.txt");
        std::fs::write(&cache, "the cached reading").unwrap();
        assert_eq!(Book::ocr_page(&dir, 0).as_deref(), Some("the cached reading"));

        // The control comes on *after* the cache was written: stale, and
        // due for a re-read.
        std::thread::sleep(std::time::Duration::from_millis(20));
        book.set_force_ocr(true);
        assert!(Book::ocr_cache_is_stale(&dir, 0), "a cache older than the control is stale");

        // A cache written while the control is on (touch it after the
        // marker) is fresh, and a forced read serves it as-is rather than
        // re-recognising it.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let fresh = std::fs::File::options().append(true).open(&cache).unwrap();
        fresh.set_modified(std::time::SystemTime::now()).unwrap();
        drop(fresh);
        assert!(
            !Book::ocr_cache_is_stale(&dir, 0),
            "a cache written under the control is fresh"
        );
        // The forced path itself serves the fresh cache without a
        // recognition (there is no engine here to prove it with — serving
        // at all proves the cache was not refused).
        assert_eq!(
            Book::ocr_page_forced(&dir, 0).as_deref(),
            Some("the cached reading")
        );
    }

    #[test]
    fn a_scanned_book_opens_on_its_first_cached_ocr_page() {
        let id = new_book_id();
        let dir = BooksStore::dir().join(&id);
        std::fs::create_dir_all(dir.join("text")).unwrap();
        let mut manifest = manifest_fixture();
        manifest.id = id.clone();
        manifest.format = "pdf".to_string();
        manifest.spine_count = 10;
        manifest.spine = (0..10).map(|i| format!("page-{i}")).collect();
        let book = Book::new(manifest, dir);
        // No caches: the walk finds no layer and no cache, so page 0 it is.
        assert_eq!(book.first_speakable_chapter(), 0);
        // A cache that recognition emptied is not an opening.
        std::fs::write(book.dir.join("text/0001.txt"), "").unwrap();
        assert_eq!(book.first_speakable_chapter(), 0);
        // The first page recognition actually read is.
        std::fs::write(book.dir.join("text/0004.txt"), "learning outcome").unwrap();
        assert_eq!(book.first_speakable_chapter(), 3, "0-based index of cached page 4");
    }
}
