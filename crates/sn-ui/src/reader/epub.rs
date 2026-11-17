//! Split out of reader.rs 2026-10-31 (pure code-motion).

use std::rc::Rc;
use gtk4::prelude::*;

use crate::app::App;
use super::*;

/// for the voice.
///
/// The web engine shows the spine item exactly as the book ships it — its
/// stylesheet, its figures, its colours — and the *parsed* chapter goes into
/// the read-along view, which is not what the reader looks at. Two surfaces,
/// one book: the split the iOS app had and the label-per-paragraph renderer
/// could not.
pub(crate) fn open_epub(app: &Rc<App>, book: &sn_store::books::Book, index: usize, text: &str) {
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    let reader = &w.reader;
    reader.pdf_tools.set_visible(false);
    // The page-progress bar belongs to a PDF: an EPUB has chapters, not
    // pages, and the reader's chapter position is already in the status.
    reader.page_bar.set_visible(false);
    reader.epub_tools.set_visible(true);
    //  the presenter is a deck's control.
    reader.present.set_visible(false);
    w.reader.slides.end_show();
    sync_show_controls(app);
    reader.title.set_text(&book.manifest.title);

    // The archive (or, for a headless document, the parse) is memoised
    // inside the Book — the open-path worker already paid for it — and the
    // scheme handler keeps a clone of the same one, so serving a chapter
    // costs a lookup. A document has no archive to open: its chapters are
    // served out of the parse, the same spine the read-along view speaks.
    let spine = book.manifest.spine.clone();
    if book.is_document() {
        match book.served() {
            Ok(parts) => reader.epub.set_parts(parts, spine),
            Err(e) => {
                set_status(app, &format!("Could not open the book: {e}"));
                return;
            }
        }
    } else {
        match book.archive() {
            Ok(archive) => reader.epub.set_book(std::sync::Arc::new(archive), spine),
            Err(e) => {
                set_status(app, &format!("Could not open the book: {e}"));
                return;
            }
        }
    }
    reader
        .epub
        .set_appearance(appearance_from_prefs(&app.prefs.get()));

    // A link inside the book moves the spine index: the reader's place, the
    // status line, the contents' active row and the manifest all follow it.
    {
        let app = app.clone();
        reader.epub.on_load(move |index| {
            if let Some(index) = index {
                on_epub_chapter_changed(&app, index);
            }
        });
    }

    reader.epub.load_spine_item(index, None);
    // The parsed chapter: the voice reads it and the read-along highlight
    // lands in it. It is computed either way — the speech path needs it — so
    // it costs nothing to keep.
    reader.epub_text.buffer().set_text(text);
    reader.stack.set_visible_child_name("book");
    reader
        .status
        .set_text(&format!("chapter {} / {}", index + 1, book.chapters()));
    //  exchanged with the TOC button's icon (user report).
    reader.mode_toggle.set_icon_name("view-contents-symbolic");
    reader
        .mode_toggle
        .set_tooltip_text(Some("Read along with the parsed text"));
}

/// The book's own link moved to a new spine item.
///
/// The same follow-the-page work `on_pdf_page_changed` does, for a chapter
/// the webview navigated to rather than one the contents named. The text of
/// the new chapter is extracted because the voice and the read-along view
/// must not keep speaking the one that just left the screen.
pub(crate) fn on_epub_chapter_changed(app: &Rc<App>, index: usize) {
    let book_id = {
        let mut state = app.reader.borrow_mut();
        if state.chapter == index {
            return;
        }
        state.chapter = index;
        state.book_id.clone()
    };
    let Some(book_id) = book_id else {
        return;
    };
    let speaking = app.reader.borrow().speaking;
    let Ok(Some(book)) = current_book(app) else {
        return;
    };
    if book.manifest.format == "pdf" {
        return;
    }
    if let Ok(full) = book.chapter_full(index) {
        app.reader.borrow_mut().text = full.text.clone();
        if let Some(w) = app.widgets.borrow().clone() {
            w.reader.epub_text.buffer().set_text(&full.text);
        }
    }
    if let Some(w) = app.widgets.borrow().clone() {
        w.reader
            .status
            .set_text(&format!("chapter {} / {}", index + 1, book.chapters()));
        mark_toc_row(app, index);
        refresh_controls(app);
    }
    if !speaking {
        schedule_save_pdf_position(app, &book_id, index);
    }
}

/// The reader's appearance, from the stored prefs.
///
/// Every knob here is a pref, and every change to one of them is applied to
/// the open book immediately by [`apply_epub_appearance`] — calibre's shape,
/// where a settings page writes through and the view re-reads.
pub fn appearance_from_prefs(p: &sn_store::Prefs) -> EpubAppearance {
    EpubAppearance {
        theme: p.reader_theme.clone(),
        font: p.reader_font.clone(),
        font_family: p.reader_font_family.clone(),
        font_size: p.reader_font_size,
        min_font_size: p.reader_min_font_size,
        letter_spacing: p.reader_letter_spacing,
        line_height: p.reader_line_height,
        paragraph_spacing: p.reader_paragraph_spacing,
        margin_top: p.reader_margin_top,
        margin_bottom: p.reader_margin_bottom,
        margin_left: p.reader_margin_left,
        margin_right: p.reader_margin_right,
        max_width: p.reader_max_width,
        flow: p.reader_flow.clone(),
        columns: p.reader_columns,
        override_colors: p.reader_override_colors.clone(),
        respect_styles: p.reader_respect_styles,
    }
}

/// Apply the stored appearance to the open book, right now.
///
/// Called by every settings control that changes a reader pref: the reader
/// never has to be closed and reopened for a change to be visible.
pub fn apply_epub_appearance(app: &Rc<App>) {
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    // Both book surfaces dress from the same prefs: the EPUB's per-spine
    // view, and the engine the headless documents open in.
    w.reader
        .epub
        .set_appearance(appearance_from_prefs(&app.prefs.get()));
    w.reader
        .book
        .set_appearance(appearance_from_prefs(&app.prefs.get()));
}

