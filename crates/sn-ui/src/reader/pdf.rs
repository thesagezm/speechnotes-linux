//! Split out of reader.rs 2026-10-31 (pure code-motion).

use std::rc::Rc;
use gtk4::glib;
use gtk4::prelude::*;

use crate::app::App;
use super::*;

/// voice.
///
/// The text is still read — the speech path needs it, the search needs it,
/// and the read-along highlight needs it — but it is not what the reader
/// looks at. The pages are the thing on screen.
pub(crate) fn open_pdf(
    app: &Rc<App>,
    book: &sn_store::books::Book,
    index: usize,
    text: &str,
    sizes: Option<Vec<(f64, f64)>>,
    links: Vec<sn_doc::pdf::PageLink>,
) {
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    let reader = &w.reader;
    reader.pdf_tools.set_visible(true);
    reader.page_bar.set_visible(true);
    //  the presenter is a deck's control.
    reader.present.set_visible(false);
    w.reader.slides.end_show();
    sync_show_controls(app);
    reader.title.set_text(&book.manifest.title);
    // The page count is the book's page count, not a chapter count: a PDF's
    // "chapter" is a page, and the reader thinks in pages.
    reader
        .status
        .set_text(&format!("page {} / {}", index + 1, book.chapters()));
    set_page_bar_fraction(reader, index, book.chapters());

    // The layout: every page's box, so the scroll knows the book's length
    // before a single page is drawn. The sizes came off the worker thread
    // with the rest of the open; the fallback is here for the paths that
    // did not have them.
    let sizes = match sizes {
        Some(sizes) if !sizes.is_empty() => sizes,
        _ => match book.page_sizes() {
            Ok(sizes) if !sizes.is_empty() => sizes,
            Ok(_) => vec![(612.0, 792.0); book.chapters()],
            Err(e) => {
                set_status(app, &format!("Could not read the page layout: {e}"));
                vec![(612.0, 792.0); book.chapters()]
            }
        },
    };
    reader
        .pdf
        .set_book(&book.manifest.id, book.original(), sizes);
    // The book's own contents page clicks: the parse's annotation table.
    // A row jumps to its page inside the view; a web address goes through
    // the same safe-link rule the EPUB view's links follow.
    reader.pdf.set_links(links);
    reader.pdf.on_external_link(move |href| {
        if crate::epubview::is_safe_link(href) {
            gtk4::UriLauncher::new(href).launch(
                gtk4::Window::NONE,
                gtk4::gio::Cancellable::NONE,
                |result| {
                    if let Err(e) = result {
                        tracing::warn!(error = %e, "the desktop refused to open a link");
                    }
                },
            );
        } else {
            tracing::warn!(href = %href, "refusing to follow a link with an unlisted scheme");
        }
    });
    reader.pdf.scroll_to_page(index);

    // The pages the reader stands on are the ones the voice follows.
    {
        let app = app.clone();
        reader.pdf.on_page_changed(move |page| {
            on_pdf_page_changed(&app, page);
        });
    }

    // The text side: the current page's text in a read-only text view, so
    // the read-along highlight has a buffer to tag.
    reader.pdf_text.buffer().set_text(text);
    reader.stack.set_visible_child_name("pages");
    reader
        .mode_toggle
        //  exchanged with the TOC button's icon (user report).
        .set_icon_name("view-contents-symbolic");
    reader.mode_toggle.set_tooltip_text(Some("Read the extracted text instead of the pages"));
    // The toggle reads the book's marker, not the other way round: the
    // toggled handler no-ops when the state already matches.
    reader.force_ocr.set_active(book.force_ocr());
}

/// The page-progress bar's fraction: `page` of `total`.
///
/// The fraction is the page the reader is *on* over the pages in the book,
/// so the first page of a hundred is a full bar's start, not an empty
/// one: the reader is at the beginning of the document, and the bar says
/// so.
///
/// A book with no pages known leaves the bar empty rather than dividing by
/// zero, and a stale page number past the end clamps to full rather than
/// drawing a bar wider than the track.
pub(crate) fn page_bar_fraction(page: usize, total: usize) -> f64 {
    if total == 0 {
        return 0.0;
    }
    ((page + 1).min(total) as f64 / total as f64).clamp(0.0, 1.0)
}

/// Put the page-progress bar at `page` of `total`.
pub(crate) fn set_page_bar_fraction(reader: &ReaderWidgets, page: usize, total: usize) {
    reader.page_bar.set_fraction(page_bar_fraction(page, total));
}

/// The reader scrolled to a new page: the reader's place, the status line,
/// the contents' active row and the manifest's position all follow.
pub(crate) fn on_pdf_page_changed(app: &Rc<App>, page: usize) {
    let book_id = {
        let mut state = app.reader.borrow_mut();
        if state.chapter == page {
            return;
        }
        state.chapter = page;
        state.book_id.clone()
    };
    let Some(book_id) = book_id else {
        return;
    };
    // The voice owns the page while it speaks: the view follows the voice,
    // not the other way round, and the page the voice is on is the page the
    // manifest records. The spoken text must stay the text on screen too —
    // the read-along highlight compares the two — so the page's text is
    // refreshed here only while the voice is quiet; the voice's own page
    // turns refill it through `open_book`.
    let speaking = app.reader.borrow().speaking;
    let Ok(Some(book)) = current_book(app) else {
        return;
    };
    if let Some(w) = app.widgets.borrow().clone() {
        w.reader
            .status
            .set_text(&format!("page {} / {}", page + 1, book.chapters()));
        set_page_bar_fraction(&w.reader, page, book.chapters());
        mark_toc_row(app, page);
        refresh_controls(app);
    }
    if !speaking {
        // The page's text is extracted off the UI thread: a five-page window
        // out of a 1295-page book is milliseconds of parsing that used to
        // land in the middle of a page turn.
        request_page_text(app, &book_id, page);
        // And the manifest's position is written **debounced**, on a worker:
        // a page turn used to rewrite the whole manifest JSON and fsync it
        // on the UI thread, every page.
        schedule_save_pdf_position(app, &book_id, page);
    }
}

/// Extract a page's text on a worker, and hand it to the read-along view
/// when it lands.
pub(crate) fn request_page_text(app: &Rc<App>, book_id: &str, page: usize) {
    let Some(manifest) = manifest_of(app, book_id) else {
        return;
    };
    let gen = app.reader.borrow().generation;
    crate::tasks::spawn(move || {
        let book = sn_store::books::BooksStore::from_manifest(manifest);
        let text = book.chapter_text(page).unwrap_or_default();
        crate::tasks::Msg::PageText { gen, page, text }
    });
}

/// The manifest of the open book, as plain data — the only thing a worker
/// is allowed to carry in.
pub(crate) fn manifest_of(app: &Rc<App>, book_id: &str) -> Option<sn_store::books::BookManifest> {
    current_book(app)
        .ok()
        .flatten()
        .filter(|b| b.manifest.id == book_id)
        .map(|b| b.manifest)
}

/// A page's text landed: put it in the read-along view, if the reader is
/// still on that page.
pub fn on_page_text(app: &Rc<App>, gen: u64, page: usize, text: String) {
    {
        let state = app.reader.borrow();
        if state.generation != gen || state.chapter != page || state.speaking {
            return;
        }
    }
    app.reader.borrow_mut().text = text.clone();
    if let Some(w) = app.widgets.borrow().clone() {
        w.reader.pdf_text.buffer().set_text(&text);
    }
}

/// The manifest's position for a PDF: the page the reader is on.
///
/// **Debounced.** The write is a whole-manifest JSON rewrite plus an atomic
/// fsync — 10–100 ms on a big book's manifest — and it used to happen on the
/// UI thread on every page turned. Now the page turn only *records* where
/// the reader is; the write happens 1.2 s later, trailing, on a worker
/// thread, and only for the page the reader actually settled on.
pub(crate) fn schedule_save_pdf_position(app: &Rc<App>, book_id: &str, page: usize) {
    if let Some(old) = app.pdf_save_source.borrow_mut().take() {
        old.remove();
    }
    let app2 = app.clone();
    let book_id = book_id.to_string();
    let source = glib::timeout_add_local(
        std::time::Duration::from_millis(1200),
        move || {
            *app2.pdf_save_source.borrow_mut() = None;
            save_pdf_position(&app2, &book_id, page);
            glib::ControlFlow::Break
        },
    );
    *app.pdf_save_source.borrow_mut() = Some(source);
}

/// Write the position now, off the UI thread.
pub(crate) fn save_pdf_position(app: &Rc<App>, book_id: &str, page: usize) {
    let Some(manifest) = manifest_of(app, book_id) else {
        return;
    };
    crate::tasks::spawn_detached(move || {
        let mut book = sn_store::books::BooksStore::from_manifest(manifest);
        book.manifest.position.chapter_index = page;
        book.manifest.position.chapter_fraction = 0.0;
        book.manifest.last_opened_at = sn_core::AppleDate::now().raw();
        let _ = sn_store::books::BooksStore::save(&book);
    });
}

