//! Position reporting: the transport labels, the bookmark writes, and the
//! fraction maths they share. Split out of `abook.rs` (Round 4, pure
//! code-motion).

use std::rc::Rc;

use gtk4::prelude::*;

use crate::app::App;

use super::{AbookHandle, ABBOOK_SAVE_MIN_INTERVAL};

pub(crate) fn save_position(app: &Rc<App>, book: &sn_store::books::Book) {
    let Some(session) = app.abook.borrow().clone() else {
        return;
    };
    let chapter_start = book
        .manifest
        .chapters
        .get(session.chapter_index)
        .map(|c| c.start_seconds)
        .unwrap_or(0.0);
    let offset_ms = session.offset_ms(chapter_start);
    // A pause, a chapter advance, the sleep timer or the end of the book all
    // leave the tick loop; a save that is due then must not be dropped.
    let stopping = session.inner.is_paused() || session.inner.is_finished();
    let due = match app.abook_save_last.get() {
        Some(at) => at.elapsed() >= ABBOOK_SAVE_MIN_INTERVAL,
        None => true,
    };
    if !due && !stopping {
        // The bookmark still moves: it is the store the resume path reads
        // first, and it coalesces its own writes.
        put_bookmark(app, &session, offset_ms);
        return;
    }
    app.abook_save_last.set(Some(std::time::Instant::now()));
    let mut manifest = book.clone();
    manifest.manifest.position.chapter_index = session.chapter_index;
    manifest.manifest.position.chapter_fraction = if book.manifest.chapters.is_empty() {
        0.0
    } else {
        (session.chapter_index as f64 + elapsed_fraction(book, session.chapter_index, offset_ms))
            / book.manifest.chapters.len() as f64
    };
    let _ = sn_store::books::BooksStore::save(&manifest);
    put_bookmark(app, &session, offset_ms);
}

/// The bookmark write, split out of [`save_position`] so a tick that skips
/// the manifest write still moves the resume point.
pub(crate) fn put_bookmark(app: &Rc<App>, session: &AbookHandle, offset_ms: u64) {
    app.bookmarks.put(
        sn_store::model::book_key(&session.book_id, session.chapter_index),
        sn_store::model::PlaybackBookmark::for_note(
            &format!("book:{}:{}", session.book_id, session.chapter_index),
            offset_ms as i64,
            0,
        ),
    );
}

/// The fraction through one chapter, from the offset in milliseconds.
pub(crate) fn elapsed_fraction(
    book: &sn_store::books::Book,
    chapter_index: usize,
    offset_ms: u64,
) -> f64 {
    let Some(chapter) = book.manifest.chapters.get(chapter_index) else {
        return 0.0;
    };
    let length = (chapter.end_seconds - chapter.start_seconds).max(0.001);
    (offset_ms as f64 / 1000.0 / length).clamp(0.0, 1.0)
}

/// The whole-book fraction: full chapters before this one plus how far into
/// it the listener is. The transport bar's progress reads this — the same
/// number `save_position` writes to the manifest as `chapterFraction`, so
/// the bar and the shelf agree.
pub(crate) fn whole_fraction(book: &sn_store::books::Book, chapter: usize, into_chapter_secs: f64) -> f64 {
    let n = book.manifest.chapters.len();
    if n == 0 {
        return 0.0;
    }
    let frac = elapsed_fraction(book, chapter, (into_chapter_secs * 1000.0) as u64);
    ((chapter as f64 + frac) / n as f64).clamp(0.0, 1.0)
}

/// mm:ss (or h:mm:ss past an hour).
pub(crate) fn clock(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

pub(crate) fn set_status(app: &Rc<App>, text: &str) {
    if let Some(w) = app.widgets.borrow().as_ref() {
        w.reader.status.set_visible(true);
        w.reader.abook_position.set_visible(false);
        w.reader.status.set_text(text);
    }
}
