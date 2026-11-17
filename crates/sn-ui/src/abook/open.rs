//! Opening an audiobook: the session spawn, the book page it takes over,
//! and the chapter menu. Split out of `abook.rs` (Round 4, pure
//! code-motion).

use std::rc::Rc;

use gtk4::prelude::*;

use crate::app::App;

use super::playback::attach_tick;
use super::progress::{clock, elapsed_fraction, save_position, set_status, whole_fraction};
use super::AbookHandle;

pub fn open(app: &Rc<App>, book_id: &str, chapter_index: usize, offset_ms: u64, rate: f64) {
    // Stop the outgoing session before the new one is spawned. `drop_session`
    // only takes the handle out of the app — during a tick-driven
    // auto-advance the calling closure still holds its own `Rc`, so the old
    // ffmpeg child, writer thread, and sink would stay alive (and audible,
    // mixed with the new chapter) until the end of the tick. The explicit
    // stop kills the child and silences the sink now; the remaining `Rc`s
    // become empty shells. The handle is cloned out before the call so no
    // `borrow()` is live across it.
    let outgoing = app.abook.borrow().clone();
    if let Some(outgoing) = outgoing.as_ref() {
        outgoing.inner.stop();
    }
    // A note's speech session owns a sink to the same device this book's pipe
    // is about to open. Nothing in the audio layer serialises the two, so two
    // live writers means interleaved audio, independent underruns, and a
    // transport bar both are trying to drive. Stopping the TTS session here is
    // the same rule `reader::open_book` already applies in the other
    // direction — one sound at a time, decided by whichever the user asked
    // for. Cloned out for the same no-borrow-across-a-call reason.
    let tts = app.tts.borrow().clone();
    if let Some(tts) = tts.as_ref() {
        tts.stop();
    }
    drop_session(app);
    let Ok(Some(book)) = sn_store::books::BooksStore::find(book_id) else {
        return;
    };
    let chapters = &book.manifest.chapters;
    let index = chapter_index.min(chapters.len().saturating_sub(1));
    let start = chapters
        .get(index)
        .map(|c| c.start_seconds + offset_ms as f64 / 1000.0)
        .unwrap_or(0.0);
    let session = match sn_tts::audiobook::AudiobookSession::start(&book.audio(), start, rate) {
        Ok(s) => s,
        Err(e) => {
            set_status(app, &format!("Could not open the audio: {e}"));
            return;
        }
    };
    *app.abook.borrow_mut() = Some(std::rc::Rc::new(AbookHandle {
        inner: session,
        book_id: book.manifest.id.clone(),
        chapter_index: index,
        offset_ms,
    }));
    // Audiobook opens used to be invisible in the logs — the one session
    // whose start could not be traced when the "already borrowed" panic was
    // being chased.
    tracing::info!(
        book = %book.manifest.id,
        chapter = index,
        offset_ms,
        "audiobook open"
    );

    // The reader state follows the session when this is the book it has
    // open. Paths that reach `open` without `open_book` — the tick's
    // auto-advance, the chapter menu, the ±15s skips, the display's own
    // prev/next — would otherwise leave `reader.chapter` behind, and the
    // next Left/Right would compute from the stale chapter (one turn
    // observed landing two chapters from where the listener stood).
    {
        let mut state = app.reader.borrow_mut();
        if state.book_id.as_deref() == Some(book.manifest.id.as_str()) {
            state.chapter = index;
        }
    }

    // Labels and controls.
    if let Some(w) = app.widgets.borrow().as_ref() {
        let reader = &w.reader;
        reader.title.set_text(&book.manifest.title);
        reader
            .status
            .set_text(chapters.get(index).map(|c| c.title.as_str()).unwrap_or(""));
        reader.speak.set_pause();
        reader.prev.set_sensitive(index > 0);
        reader.next.set_sensitive(index + 1 < chapters.len());
        reader.abook_position.set_visible(true);
        reader.status.set_visible(false);
        reader
            .abook
            .chapter_bar
            .set_fraction(elapsed_fraction(&book, index, offset_ms));
        build_chapter_menu(app, &book, index);
    }
    show_book_page(app, &book, index);
    attach_tick(app);
    save_position(app, &book);
    // The transport bar is the app's one transport: show the book in it, with
    // the audiobook's own rate in the speed spin rather than the note
    // player's. A resume opens partway into the chapter, so the readouts say
    // where the listener is landing, not 00:00.
    // The page's readout is chapter-scoped; the transport bar describes the
    // whole book — its time and its fill must agree on scope, and a listener
    // nine hours in wants "9:12:40 / 22:48:00", not the chapter's.
    let length = chapters
        .get(index)
        .map(|c| c.end_seconds - c.start_seconds)
        .unwrap_or(0.0);
    let whole_elapsed = chapters.get(index).map(|c| c.start_seconds).unwrap_or(0.0)
        + (offset_ms as f64 / 1000.0).min(length.max(0.0));
    let whole_total = chapters.last().map(|c| c.end_seconds).unwrap_or(0.0);
    crate::player::describe_audiobook(
        app,
        &book.manifest.title,
        chapters.get(index).map(|c| c.title.as_str()).unwrap_or(""),
        &clock(whole_elapsed),
        &clock(whole_total),
        whole_fraction(&book, index, offset_ms as f64 / 1000.0),
    );
    crate::player::show_audiobook_speed(app);
    crate::player::set_audiobook_play_icon(app, false);
}

/// Fill the audiobook display page: the cover, what is playing, and its
/// transport.
///
/// The reader's chapter-text scroll is the EPUB's page; an audiobook has no
/// text, so the page beneath the toolbar is the book display — a big cover,
/// title and author, the chapter now playing, and the transport. The chapter
/// scroll is hidden while this page shows, and both flip back when the book
/// closes or an EPUB opens (`close` / `reader::open_book`).
pub(crate) fn show_book_page(app: &Rc<App>, book: &sn_store::books::Book, chapter_index: usize) {
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    let abook = &w.reader.abook;
    abook.page.set_visible(true);
    // The reader's body is a stack now — the EPUB's text, the PDF's pages —
    // so the whole stack goes, not just the chapter scroll that used to be
    // the only page there was.
    w.reader.stack.set_visible(false);

    abook.title.set_text(&book.manifest.title);
    abook.author.set_text(if book.manifest.author.is_empty() {
        ""
    } else {
        &book.manifest.author
    });
    let chapter_title = book
        .manifest
        .chapters
        .get(chapter_index)
        .map(|c| c.title.clone())
        .unwrap_or_default();
    abook.chapter.set_text(&chapter_title);
    let length = book
        .manifest
        .chapters
        .get(chapter_index)
        .map(|c| c.end_seconds - c.start_seconds)
        .unwrap_or(0.0);
    abook
        .position
        .set_text(&format!("00:00 / {}", clock(length)));
    abook
        .prev
        .set_sensitive(chapter_index > 0);
    abook
        .next
        .set_sensitive(chapter_index + 1 < book.manifest.chapters.len());

    // The cover: the extracted file when there is one, the placeholder
    // otherwise. `Picture::for_file` loads lazily off the paintable, so a
    // 900 KB cover does not block the open.
    let cover_path = book.cover();
    match cover_path.filter(|p| p.exists()) {
        Some(path) => {
            abook.cover.set_file(Some(&gtk4::gio::File::for_path(&path)));
            abook.cover.set_visible(true);
            abook.cover_placeholder.set_visible(false);
        }
        None => {
            abook.cover.set_visible(false);
            abook.cover_placeholder.set_visible(true);
        }
    }
    // The display's speed spin shows the same rate the transport bar's does,
    // through the same guard: a programmatic set is not the dial turning.
    let rate = app.prefs.get().audio_rate;
    abook.speed.set_value(rate);
    app.shown_speed.set(rate);
}

/// Take the audiobook display down: the chapter scroll returns, the page
/// hides. The inverse of [`show_book_page`].
///
/// Stopping playback must NOT come through here — a stopped audiobook keeps
/// its cover and controls on screen (the reader will want to press play
/// again), so only the *opening of something else* takes the display down:
/// `reader::open_book` calls this on its way to an EPUB or a PDF.
pub fn hide_book_page(app: &Rc<App>) {
    if let Some(w) = app.widgets.borrow().as_ref() {
        w.reader.abook.page.set_visible(false);
        w.reader.stack.set_visible(true);
    }
}

/// The chapter list. It lives in the reader's contents sidebar now (see
/// `reader/toc.rs`) — flat rows, one per chapter — instead of a popover on
/// its own X surface, which sat beyond the a11y tree's click paths. The
/// player calls on every chapter move: the rows are refilled only when the
/// book changes, and the chapter now playing is marked every time.
pub(crate) fn build_chapter_menu(app: &Rc<App>, book: &sn_store::books::Book, current: usize) {
    crate::reader::fill_chapter_rows(app, book, current);
}

/// Play/pause.
pub fn toggle(app: &Rc<App>) {
    let session = app.abook.borrow().clone();
    let Some(session) = session else { return };
    let paused = session.inner.is_paused();
    session.inner.set_paused(!paused);
    if let Some(w) = app.widgets.borrow().as_ref() {
        // The glyph follows the *new* state: `paused` was the state before
        // the toggle, so the session is playing again when `paused` was
        // true.
        w.reader.speak.set_playing(paused);
        // The book display's play button is the same session's.
        w.reader.abook.play.set_playing(paused);
    }
    crate::player::set_audiobook_play_icon(app, !paused);
}

/// Take the session out of the app and drop it, so the ffmpeg child and the
/// sink go away now rather than when the last `Rc` clone does.
pub(crate) fn drop_session(app: &Rc<App>) {
    let taken = app.abook.borrow_mut().take();
    if let Some(session) = taken {
        let _ = session;
    }
    if let Some(source) = app.abook_tick.borrow_mut().take() {
        source.remove();
    }
    *app.abook_sleep.borrow_mut() = None;
}

/// Stop and drop the session.
///
/// The book display stays exactly where it is — cover, chapter, transport,
/// all of it — because a stopped audiobook is still the open book; tearing
/// the page down here is what made the stop button throw the listener back
/// onto whatever ebook they had open before. Only the icons reset, so the
/// page's play button reads as "play again", and it does: `maybe_toggle`
/// re-opens the book from its bookmark when there is no session to pause.
pub fn close(app: &Rc<App>) {
    drop_session(app);
    // The transport bar belongs to the note player again; leaving the last
    // book's title in it made the bar describe something that is not playing.
    crate::player::refresh_transport(app);
    if let Some(w) = app.widgets.borrow().as_ref() {
        w.reader.speak.set_play();
        w.reader.abook.play.set_play();
    }
}
