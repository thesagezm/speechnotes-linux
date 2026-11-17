//! Split out of reader.rs 2026-10-31 (pure code-motion).

use std::rc::Rc;
use gtk4::prelude::*;

use crate::app::App;
use super::*;

/// Speak the open chapter — from the page on screen for an engine book,
/// from the chapter's bookmark otherwise.
pub fn speak_chapter(app: &Rc<App>) {
    if matches!(current_book(app), Ok(None) | Err(_)) {
        return;
    }
    // A user asking for the voice (F5, the transport) starts where THEY are
    // looking: any walk the last session left under way ends here, and the
    // next walk resumes from the page on screen. The walk's own entries
    // bypass this — `speak_chapter_from` directly — so the walk's position
    // survives its own steps.
    app.reader.borrow_mut().walk_section = None;
    if app.reader.borrow().engine_rendered {
        // Until the engine is ready, the state's text is still the old
        // parse's seed — the very thing the engine exists to replace — so
        // the ask waits for the engine's own report instead of speaking it.
        if !app.reader.borrow().engine_ready {
            let gen = app.reader.borrow().generation;
            app.reader.borrow_mut().speak_when_opened = Some(gen);
            return;
        }
        // The engine's page is where reading starts: ask where the visible
        // page begins in its section's text and start there, not at the
        // section's top — a section can run a hundred pages past the one
        // the reader is looking at. The answer lands on the UI thread a
        // moment later; the section's text is the state's by then (its own
        // report wrote it before the voice could be asked to speak).
        if let Some(w) = app.widgets.borrow().clone() {
            let callback_app = app.clone();
            w.reader.book.page_offset(move |index, offset| {
                on_page_offset(&callback_app, index, offset);
            });
        }
        return;
    }
    speak_chapter_from(app, None);
}

/// The engine's answer to "where does this page start": the voice starts
/// there. `offset` is UTF-16 units into the section's text — the unit the
/// bookmarks store.
fn on_page_offset(app: &Rc<App>, index: usize, offset: Option<usize>) {
    let (chapter, text) = {
        let state = app.reader.borrow();
        (state.chapter, state.text.clone())
    };
    // The view's section and the state's must agree — a navigation racing
    // the ask leaves the pair inconsistent, and the section top (offset
    // zero into the state's own text) is the honest fallback.
    let resume = if index == chapter {
        offset
            .map(|units| sn_store::model::utf16_to_byte_offset(&text, units as i64))
            .unwrap_or(0)
    } else {
        0
    };
    speak_chapter_from(app, Some(resume));
}

/// The chapter's voice. `resume` is a byte offset into the open chapter's
/// text when the caller knows where to start (the engine's visible page);
/// `None` seeds from the chapter's bookmark, exactly as before.
pub(crate) fn speak_chapter_from(app: &Rc<App>, resume: Option<usize>) {
    let Ok(Some(book)) = current_book(app) else {
        return;
    };
    let (chapter, text, engine) = {
        let state = app.reader.borrow();
        (state.chapter, state.text.clone(), state.engine_rendered)
    };
    if text.trim().is_empty() || !sn_format::sanitizer::is_speakable(&text) {
        if engine {
            // The engine's own sections decide: its report starts the voice
            // when one with words lands, and the walk passes the silent
            // ones by — the old parse's chapter file has no say on a book
            // the engine renders.
            crate::player::advance_engine_voice(app, &book.manifest.id);
            return;
        }
        // A scanned page the voice cannot read is not the end: the voice
        // looks ahead — on a worker, a cold page's recognition costs a
        // second — and starts on the first page with text.
        set_status(app, "This page has no text — looking ahead for the first one that does…");
        crate::player::speak_from_first_speakable(book.manifest.id.clone(), chapter);
        return;
    }
    let resume = match resume {
        Some(bytes) => {
            if !sn_format::sanitizer::is_speakable(text.get(bytes..).unwrap_or_default()) {
                set_status(app, "Nothing left to speak from there.");
                return;
            }
            bytes
        }
        // The bookmark for this chapter seeds the resume byte; the session's
        // own frontier takes over from the first tick, exactly as a note does.
        None => app
            .bookmarks
            .for_book_chapter(&book.manifest.id, chapter)
            .map(|b| sn_store::model::utf16_to_byte_offset(&text, b.text_offset))
            .unwrap_or(0),
    };
    if let Err(e) = crate::player::start_session_for_text(app, book.manifest.id.clone(), text, resume) {
        set_status(app, &e);
    }
}

/// The transport's play on a book page: `true` when the reader is showing a
/// book the note player must not speak — the voice starts on the book
/// instead, from the page on screen. An audiobook is not this path's
/// business (`maybe_toggle` routed it), and a live session neither (the
/// transport's play/pause owns a session that is playing or paused).
pub fn speak_open_book(app: &Rc<App>) -> bool {
    let reader_page = app
        .widgets
        .borrow()
        .as_ref()
        .and_then(|w| w.editor_stack.visible_child_name())
        .as_deref()
        == Some("reader");
    if !reader_page {
        return false;
    }
    let is_book = {
        let state = app.reader.borrow();
        state.book_id.is_some() && state.format != "audiobook"
    };
    if !is_book {
        return false;
    }
    speak_chapter(app);
    true
}

/// Speak the PDF from `page` (0-based) — the pages view's "Speak from here".
///
/// Speak-from-selection: a rendered page has no
/// selectable text, so the right-click on the page *is* the selection, and
/// speech starts from that page's first byte. The page's text is extracted
/// on a worker (a cold scan page's recognition costs a second); the answer
/// lands through `tasks::Msg::SpeakPage` and starts the session. A page with
/// no text looks ahead for the first one that has some, exactly as
/// `speak_chapter` does.
pub fn speak_from_page(app: &Rc<App>, page: usize) {
    let Ok(Some(book)) = current_book(app) else {
        return;
    };
    let gen;
    {
        let mut state = app.reader.borrow_mut();
        state.chapter = page;
        gen = state.generation;
    }
    if let Some(w) = app.widgets.borrow().clone() {
        w.reader.pdf.scroll_to_page(page);
        w.reader
            .status
            .set_text(&format!("page {} / {}", page + 1, book.chapters()));
        mark_toc_row(app, page);
    }
    let manifest = book.manifest.clone();
    crate::tasks::spawn(move || {
        let book = sn_store::books::BooksStore::from_manifest(manifest);
        let text = book.chapter_text(page).unwrap_or_default();
        crate::tasks::Msg::SpeakPage { gen, page, text }
    });
}

/// The page the reader's voice is on, for the PDF's text mode.
pub fn pdf_text_view(app: &Rc<App>) -> Option<gtk4::TextView> {
    let state = app.reader.borrow();
    if state.format != "pdf" {
        return None;
    }
    let w = app.widgets.borrow().clone()?;
    Some(w.reader.pdf_text.clone())
}

/// The buffer the read-along highlight belongs in, when the reader has one.
///
/// An EPUB's parsed chapter and a PDF's extracted text are the same shape —
/// a read-only text view the voice's text was written into — so the
/// highlight's home is whichever of the two is open, and the editor's buffer
/// is the fallback when neither is.
///
/// A deck is different: the slide itself is the highlight's home (a webview,
/// not a buffer — the player asks the view directly). The text view here
/// carries the tag only while the reader is looking at it, so the text page
/// returns it and the slides page returns `None`.
pub fn read_along_target(app: &Rc<App>) -> Option<gtk4::TextView> {
    let state = app.reader.borrow();
    if !app.tts_book.get() {
        return None;
    }
    let format = state.format.as_str();
    if is_slides_format(format) {
        let w = app.widgets.borrow().clone()?;
        let visible = w.reader.stack.visible_child_name()?;
        if visible.as_str() != "slides-text" {
            return None;
        }
        return Some(w.reader.slides_text.clone());
    }
    if format != "epub"
        && format != "pdf"
        && !sn_doc::bookdoc::DocFamily::is_document_extension(format)
    {
        return None;
    }
    let w = app.widgets.borrow().clone()?;
    Some(if format == "pdf" {
        w.reader.pdf_text.clone()
    } else {
        // An EPUB and the headless documents alike: the read-along view
        // beside the webview, speaking the parsed chapter.
        w.reader.epub_text.clone()
    })
}

/// True when the reader is looking at a PDF's extracted text.
pub fn in_pdf_text_mode(app: &Rc<App>) -> bool {
    let state = app.reader.borrow();
    state.format == "pdf" && state.pdf_text_mode
}

/// The PDF's page view, when a PDF is open.
pub fn pdf_view(app: &Rc<App>) -> Option<crate::pdfview::PdfView> {
    let state = app.reader.borrow();
    if state.format != "pdf" {
        return None;
    }
    let w = app.widgets.borrow().clone()?;
    Some(w.reader.pdf.clone())
}

/// Whether the voice owns the reader's page.
///
/// While the voice is speaking, the page it is on is the reader's page — the
/// speech path reads the page's own text — so the view follows the voice and
/// the reader's scroll does not move the manifest's position under it.
pub fn set_speaking(app: &Rc<App>, speaking: bool) {
    {
        let mut state = app.reader.borrow_mut();
        state.speaking = speaking;
    }
    // The page bar glows while the voice is moving — the same `live` class
    // the transport bars carry, and the same rule: the class is added and
    // removed by the code that already knows the state, so no timer exists
    // to switch it on.
    if let Some(w) = app.widgets.borrow().clone() {
        if speaking {
            if !w.reader.page_bar.has_css_class("live") {
                w.reader.page_bar.add_css_class("live");
            }
        } else if w.reader.page_bar.has_css_class("live") {
            w.reader.page_bar.remove_css_class("live");
        }
    }
}

/// Whether the reader page is the visible one of the editor stack.
pub fn page_visible(app: &Rc<App>) -> bool {
    app.widgets
        .borrow()
        .as_ref()
        .and_then(|w| w.editor_stack.visible_child_name())
        .as_deref()
        == Some("reader")
}

