//! Split out of reader.rs 2026-10-31 (pure code-motion).

use std::rc::Rc;

use crate::app::App;
use super::*;

/// The open book is gone — deleted out from under the reader. The state
/// stands down to what [`ReaderState::default`] starts with, so the next
/// editor refresh shows the "no note selected" page instead of a reader
/// still pointing at a book whose directory no longer exists.
///
/// This is the close path the reader never had (nothing else ever cleared
/// `book_id`): a book's life ended only when the app did.
pub fn forget_open_book(app: &Rc<App>) {
    let mut state = app.reader.borrow_mut();
    *state = ReaderState::default();
    // The voice must not keep a plan for a book that is gone; the player
    // stops on the next tick with nothing to play.
    state.speaking = false;
}

/// Open a book at `chapter`, filling the pane with its text.
///
/// The work happens **off the UI thread**: finding the book, reading its
/// manifest, parsing a PDF (seconds, for a big one), extracting the first
/// page's text and asking for every page's box all used to run here, and
/// the window was white for all of it. The reader page now appears
/// immediately with an "Opening…" status, and the worker posts the result
/// back; [`on_book_opened`] puts it on screen.
///
/// The generation counter is the guard: a second book opened while the
/// first is still loading wins, and the first's result is dropped.
///
/// Returns the open's generation, so a caller that wants the open to end in
/// speech (`speak_when_opened`) can stamp the flag with exactly this open
/// and no other.
pub fn open_book(app: &Rc<App>, book_id: &str, chapter: usize) -> u64 {
    let gen = {
        let mut state = app.reader.borrow_mut();
        state.generation += 1;
        state.generation
    };
    // The page shows now, the content lands when it is ready.
    crate::window::show_reader_page(app);
    set_status(app, "Opening…");
    let book_id = book_id.to_string();
    let chapter = if chapter == RESUME {
        crate::reader::RESUME
    } else {
        chapter
    };
    crate::tasks::spawn(move || open_book_worker(gen, book_id, chapter));
    gen
}

/// The worker half of [`open_book`]. It owns no widget and borrows no
/// `Rc`: everything it sends back is plain data.
pub(crate) fn open_book_worker(gen: u64, book_id: String, chapter: usize) -> crate::tasks::Msg {
    let Ok(Some(book)) = sn_store::books::BooksStore::find(&book_id) else {
        return crate::tasks::Msg::BookOpened {
            gen,
            result: Err(format!("No book matches {book_id:?}")),
        };
    };
    // An audiobook is not this path's business: the player opens it, with
    // its own bookmark resume. Say so and let the UI thread run that.
    if book.is_audiobook() {
        return crate::tasks::Msg::AudiobookOpened {
            gen,
            manifest: book.manifest.clone(),
        };
    }
    // `RESUME` is the saved position when there is one, else the first
    // chapter with prose — chapter 0 is a cover page in most books.
    let index = if chapter == RESUME {
        let saved = book.manifest.position.chapter_index;
        if saved > 0 {
            saved
        } else {
            book.first_speakable_chapter()
        }
    } else {
        chapter
    };
    // A headless document opens in the engine, whose sections are the
    // format's own (a KF8's NCX, a TXT's detected headings) — not the Rust
    // parse's chapters, which only feed the read-along view. Its index is
    // used as-is, and the parsed text at that index is best-effort: the
    // engine's own text report replaces it when the section lands.
    let engine = book.is_document();
    let index = if engine {
        index
    } else {
        index.min(book.chapters().saturating_sub(1))
    };
    let text = match book.chapter_text(index) {
        Ok(text) => text,
        Err(e) if engine => {
            tracing::info!(index, error = %e, "the parse has no text at the engine's section — the engine's report will supply it");
            String::new()
        }
        Err(e) => {
            return crate::tasks::Msg::BookOpened {
                gen,
                result: Err(format!("Could not read chapter {index}: {e}")),
            }
        }
    };
    // A PDF's page boxes are asked for here, off the UI thread, from the
    // same parse the text came from — the viewer's layout needs them before
    // it can scroll to the resume page. The link annotations ride along:
    // the book's own contents page has to click, and the parse is the only
    // thing that knows where its rows sit.
    let (sizes, links) = if book.manifest.format == "pdf" {
        (
            book.page_sizes().ok().filter(|s| !s.is_empty()),
            book.page_links(),
        )
    } else {
        (None, Vec::new())
    };
    crate::tasks::Msg::BookOpened {
        gen,
        result: Ok(crate::tasks::Opened {
            manifest: book.manifest.clone(),
            index,
            text,
            sizes,
            links,
        }),
    }
}

/// A finished book open, on the UI thread.
///
/// The generation check is what makes an out-of-order pair of opens safe:
/// only the newest one's result is applied.
pub fn on_book_opened(
    app: &Rc<App>,
    gen: u64,
    result: Result<crate::tasks::Opened, String>,
) {
    if app.reader.borrow().generation != gen {
        // A newer open has already been asked for; this answer is stale.
        return;
    }
    let opened = match result {
        Ok(opened) => opened,
        Err(e) => {
            set_status(app, &e);
            return;
        }
    };
    let crate::tasks::Opened {
        manifest,
        index,
        text,
        sizes,
        links,
    } = opened;
    let book = sn_store::books::BooksStore::from_manifest(manifest);
    // An EPUB or a PDF must not leave an audiobook playing underneath it:
    // the session would keep its ffmpeg child and its sink alive, and the
    // transport keys would still drive it. Closing the session leaves the
    // display up (a stopped book keeps its page), so the display comes down
    // here, on the way to a book that uses the chapter scroll.
    crate::abook::close(app);
    crate::abook::hide_book_page(app);
    {
        let mut state = app.reader.borrow_mut();
        state.book_id = Some(book.manifest.id.clone());
        state.chapter = index;
        state.text = text.clone();
        state.format = book.manifest.format.clone();
        state.pdf_text_mode = false;
        // The engine flags belong to the route below: a PDF or an EPUB on
        // the per-spine view is not engine-rendered (the engine's own flag
        // stays — a document re-open checks it to navigate rather than
        // reload).
        state.engine_rendered = false;
    }
    // The manifest's position is what the shelf shows and the next launch
    // resumes from; the fraction starts at zero on an explicit open.
    let mut saved = book.clone();
    saved.manifest.position.chapter_index = index;
    saved.manifest.position.chapter_fraction = 0.0;
    saved.manifest.last_opened_at = sn_core::AppleDate::now().raw();
    let _ = sn_store::books::BooksStore::save(&saved);

    tracing::info!(
        book = %book.manifest.id,
        chapter = index,
        chars = text.chars().count(),
        "reader opened a chapter"
    );
    // A scanned book's pages are recognised in the background while the
    // user reads: by the time the voice or a search reaches a page, its
    // text is a cache hit rather than a second-long wait. Opening another
    // book stops this walk through the shared flag.
    if book.manifest.format == "pdf" {
        warm_ocr(app, &book.manifest.id);
    }
    crate::window::show_reader_page(app);
    if book.manifest.format == "pdf" {
        open_pdf(app, &book, index, &text, sizes, links);
    } else if book.is_slides() {
        open_slides(app, &book, index, &text);
    } else if book.is_document() {
        // A headless document — the Mobipocket/Kindle family, FB2, RTF,
        // HTML, plain text — opens in the engine: the file's own bytes into
        // foliate-js, the book as the book out of it. The parsed text below
        // (`text`) seeds the read-along view until the engine's own text
        // report lands.
        open_document(app, &book, index, &text);
    } else {
        // An EPUB is served from its archive; the appearance layer and the
        // read-along view dress it.
        open_epub(app, &book, index, &text);
    }
    build_toc(app, &book);
    refresh_controls(app);
    // An auto-advance or a forward walk asked for this open: the voice
    // starts on the chapter that just landed — its text is the state's now,
    // which is the whole point of the flag. Only the generation the flag
    // was stamped with speaks; a newer open (the user's own click) does not
    // inherit it. The engine-rendered documents hold the flag instead: the
    // voice starts when *their* section text lands (see `on_section_text`),
    // because the parsed text the worker fetched is not what the engine
    // renders.
    let engine = book.is_document();
    let speak_now = {
        let mut state = app.reader.borrow_mut();
        if engine {
            false
        } else if state.speak_when_opened == Some(gen) {
            state.speak_when_opened.take().is_some()
        } else {
            false
        }
    };
    if speak_now {
        speak_chapter(app);
    }
}

/// The background recognition pass over a PDF's pages: every page whose
/// layer is empty or junk gets OCR'd and cached while the user reads, so
/// the voice's walk and a search meet cache hits instead of waits — and,
/// since the mix, every page near the reader is recognised so its layer and
/// its recognition can be spoken together. Fire and forget — the thread owns
/// its own book handle (a `Book` cannot cross threads), and the parse memo
/// keeps it off the UI thread's parse. Each page reports back as it lands:
/// a 46-page warm-up is twenty-plus seconds of work, and "recognising page
/// 12 / 46" is the difference between a visible process and a frozen app.
pub(crate) fn warm_ocr(app: &Rc<App>, book_id: &str) {
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    app.ocr_warmup_stop.set(stop.clone());
    let book_id = book_id.to_string();
    crate::tasks::spawn_detached(move || {
        let Ok(Some(book)) = sn_store::books::BooksStore::find(&book_id) else {
            return;
        };
        let id = book_id.clone();
        let recognised = book.ocr_warmup(&stop, move |done, total, page| {
            crate::tasks::report(crate::tasks::Msg::OcrProgress {
                book_id: id.clone(),
                done,
                total,
                page,
            });
        });
        if recognised > 0 {
            tracing::info!(book = %book_id, pages = recognised, "ocr warm-up recognised pages");
        }
        crate::tasks::report(crate::tasks::Msg::OcrDone {
            book_id,
            recognised,
        });
    });
}

/// The warm-up's per-page report: the status line announces the page just
/// recognised. Only this book's walk touches this book's line — another
/// book opened mid-walk stops the walk anyway, and the message for a
/// superseded book is dropped here.
pub fn on_ocr_progress(app: &Rc<App>, book_id: &str, done: usize, total: usize, page: usize) {
    if app.reader.borrow().book_id.as_deref() != Some(book_id) {
        return;
    }
    set_status(app, &format!("recognising {done}/{total} — page {page}"));
}

/// The warm-up finished: the ordinary page status comes back.
/// `refresh_controls` writes the page line the reader owns; a walk that
/// recognised nothing (every page already cached) needs no announcement.
pub fn on_ocr_done(app: &Rc<App>, book_id: &str, recognised: usize) {
    if app.reader.borrow().book_id.as_deref() != Some(book_id) {
        return;
    }
    if recognised > 0 {
        set_status(
            app,
            &format!(
                "recognised {recognised} new page{}",
                if recognised == 1 { "" } else { "s" }
            ),
        );
    }
    refresh_controls(app);
}

/// The audiobook branch of the open path: the worker said "this is an
/// audiobook", and the player opens it from the manifest.
pub fn on_audiobook_opened(
    app: &Rc<App>,
    gen: u64,
    manifest: sn_store::books::BookManifest,
) {
    if app.reader.borrow().generation != gen {
        return;
    }
    let book = sn_store::books::BooksStore::from_manifest(manifest);
    let (saved_chapter, saved_offset) = crate::abook::resume_point(app, &book);
    let rate = app.prefs.get().audio_rate;
    {
        let mut state = app.reader.borrow_mut();
        state.book_id = Some(book.manifest.id.clone());
        state.chapter = saved_chapter;
        state.text = String::new();
        state.format = "audiobook".into();
        state.pdf_text_mode = false;
    }
    crate::window::show_reader_page(app);
    crate::abook::open(app, &book.manifest.id, saved_chapter, saved_offset, rate);
}


/// The `--open-book` directive, set by the binary before the window builds.
static PENDING_OPEN: std::sync::OnceLock<String> = std::sync::OnceLock::new();
/// The chapter `--open-book` asked for, when it named one.
static PENDING_CHAPTER: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

/// Record a book-id prefix to open once the window exists.
pub fn set_pending_open(prefix: &str) {
    let _ = PENDING_OPEN.set(prefix.to_string());
}

/// Is a book waiting to be opened? The window asks this to decide which view
/// to start on: a book opened on the command line is a books-view thing, not a
/// notes-list thing.
pub fn has_pending_open() -> bool {
    PENDING_OPEN.get().is_some()
}

/// Record the chapter to open, overriding the book's saved position.
///
/// A PDF's "chapter" is a page, so this is also how a page is opened
/// directly: `--open-book <id> 233` opens page 233.
pub fn set_pending_chapter(chapter: usize) {
    let _ = PENDING_CHAPTER.set(chapter);
}

/// Serve a pending `--open-book` at the end of window construction. The
/// chapter is the one the command line named, if any; otherwise `RESUME` —
/// `open_book` resolves that to the book's saved position (bookmark offset
/// included, for an audiobook) or its first speakable chapter.
pub fn serve_pending_open(app: &Rc<App>) {
    let Some(prefix) = PENDING_OPEN.get() else {
        return;
    };
    let chapter = match PENDING_CHAPTER.get() {
        Some(chapter) => *chapter,
        None => RESUME,
    };
    tracing::info!(prefix, explicit = PENDING_CHAPTER.get().is_some(), "opening book");
    open_book(app, prefix, chapter);
}
