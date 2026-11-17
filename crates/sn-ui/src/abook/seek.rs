//! Seeking: a seek is a respawn — the pipe's start offset is fixed at
//! spawn. Split out of `abook.rs` (Round 4, pure code-motion).

use std::rc::Rc;

use crate::app::App;

use super::open::{close, open};
use super::progress::save_position;

pub fn shutdown(app: &Rc<App>) {
    // Belt and braces: the liveness check clones out and drops the borrow
    // before `save_position` runs, so nothing this function calls can find
    // a live `borrow()` — the shape of the close-time panic this guards
    // against.
    let live = app.abook.borrow().clone().is_some();
    if live {
        //  the original calls `save_position(app)` here, but the
        // function this version carries takes the book; the close-time save
        // re-reads it the way the tick path does.
        if let Ok(Some(book)) = crate::reader::current_book(app) {
            save_position(app, &book);
        }
    }
    close(app);
}

/// Nudge the position by `seconds` within the current chapter.
///
/// A seek is a respawn — the pipe's start offset is fixed at spawn — so this
/// is an `open` at the nudged offset, the same honest shape (and cost) as a
/// chapter change. The target clamps inside the chapter, a half second short
/// of its end: landing exactly on the end would read as finished and fire
/// the auto-advance the listener did not ask for.
pub fn skip(app: &Rc<App>, seconds: f64) {
    let Some(session) = app.abook.borrow().clone() else {
        return;
    };
    let Ok(Some(book)) = sn_store::books::BooksStore::find(&session.book_id) else {
        return;
    };
    let Some(ch) = book.manifest.chapters.get(session.chapter_index) else {
        return;
    };
    let length = (ch.end_seconds - ch.start_seconds).max(0.0);
    let now = session.offset_ms(ch.start_seconds) as f64 / 1000.0;
    let target = (now + seconds).clamp(0.0, (length - 0.5).max(0.0));
    seek_to(app, &session.book_id, session.chapter_index, target);
}

/// Seek to a fraction through the current chapter — the book page's chapter
/// bar being scrubbed. The same clamp `skip` uses: a half second short of
/// the chapter's end, so a scrub to the far edge does not read as finished
/// and fire the auto-advance.
pub fn seek_in_chapter(app: &Rc<App>, frac: f64) {
    let Some((book_id, chapter)) = seek_target_book(app) else {
        return;
    };
    let Ok(Some(book)) = sn_store::books::BooksStore::find(&book_id) else {
        return;
    };
    let Some(ch) = book.manifest.chapters.get(chapter) else {
        return;
    };
    let length = (ch.end_seconds - ch.start_seconds).max(0.0);
    let target = (frac.clamp(0.0, 1.0) * length).min((length - 0.5).max(0.0)).max(0.0);
    seek_to(app, &book_id, chapter, target);
}

/// Seek to a fraction through the whole book — the transport bar's fill
/// being scrubbed. The fraction is split with the arithmetic
/// `whole_fraction` runs in reverse: full chapters before the one the target
/// lands in, then the offset into it. A target exactly on a boundary lands
/// in the later chapter, at its top.
pub fn seek_whole(app: &Rc<App>, frac: f64) {
    let Some((book_id, _)) = seek_target_book(app) else {
        return;
    };
    let Ok(Some(book)) = sn_store::books::BooksStore::find(&book_id) else {
        return;
    };
    let chapters = &book.manifest.chapters;
    let total = chapters.last().map(|c| c.end_seconds).unwrap_or(0.0);
    if total <= 0.0 {
        return;
    }
    let target = frac.clamp(0.0, 1.0) * total;
    let mut index = 0;
    for (i, c) in chapters.iter().enumerate() {
        if c.start_seconds <= target {
            index = i;
        } else {
            break;
        }
    }
    let length = (chapters[index].end_seconds - chapters[index].start_seconds).max(0.0);
    let into = ((target - chapters[index].start_seconds).max(0.0))
        .min((length - 0.5).max(0.0));
    seek_to(app, &book_id, index, into);
}

/// The book a scrub should drive, and the chapter to treat as current.
///
/// A live session decides. Without one, the stopped book's display is still
/// up — the reader page showing an audiobook — and scrubbing it opens the
/// book at the target (and plays; there is no pause state to keep). A note
/// session must veto the fallback: the transport bar is then the note
/// player's, and dragging it must not start a book over the speaking note.
pub(crate) fn seek_target_book(app: &Rc<App>) -> Option<(String, usize)> {
    if let Some(session) = app.abook.borrow().clone() {
        return Some((session.book_id.clone(), session.chapter_index));
    }
    if app.tts.borrow().is_some() {
        return None;
    }
    let reader_page = app
        .widgets
        .borrow()
        .as_ref()
        .and_then(|w| w.editor_stack.visible_child_name())
        .as_deref()
        == Some("reader");
    if !reader_page {
        return None;
    }
    let state = app.reader.borrow();
    if state.format != "audiobook" {
        return None;
    }
    let id = state.book_id.clone()?;
    let book = sn_store::books::BooksStore::find(&id).ok().flatten()?;
    Some((id, book.manifest.position.chapter_index))
}

/// The shared tail of every scrub seek: `open` at the computed point,
/// keeping the listener's pause state. A seek checks a position — it does
/// not press play. `open` paints the playing icons; a paused seek paints
/// them back here rather than waiting a tick for the listener to see it.
pub(crate) fn seek_to(app: &Rc<App>, book_id: &str, chapter_index: usize, into_secs: f64) {
    let paused = app
        .abook
        .borrow()
        .as_ref()
        .map(|s| s.inner.is_paused())
        .unwrap_or(false);
    open(
        app,
        book_id,
        chapter_index,
        (into_secs * 1000.0) as u64,
        app.prefs.get().audio_rate,
    );
    if paused {
        if let Some(session) = app.abook.borrow().as_ref() {
            session.inner.set_paused(true);
        }
        if let Some(w) = app.widgets.borrow().as_ref() {
            w.reader.speak.set_play();
            w.reader.abook.play.set_play();
        }
        crate::player::set_audiobook_play_icon(app, true);
    }
}

/// Re-open the current chapter, keeping the listener's place.
///
/// The transport's restart button used this to go back to 0:00, but the
/// speed dials route through it too — and a listener three hours into a
/// chapter who nudges the speed must not lose their place (VLC preserves
/// position across rate changes; so does the note player's restart). The
/// heard position is the bookmark `save_position` already wrote every tick,
/// so the resume path re-reads it. Starting a chapter over from the top is
/// `restart_from_beginning` below.
pub fn restart(app: &Rc<App>) {
    let Some(session) = app.abook.borrow().clone() else {
        return;
    };
    let book_id = session.book_id.clone();
    let chapter_index = session.chapter_index;
    let chapter_start = sn_store::books::BooksStore::find(&book_id)
        .ok()
        .flatten()
        .and_then(|book| book.manifest.chapters.get(chapter_index).cloned())
        .map(|c| c.start_seconds)
        .unwrap_or(0.0);
    let offset_ms = session.offset_ms(chapter_start);
    // The position is already in the bookmark store (the tick writes it every
    // 250 ms); dropping the session must not clobber it with an earlier value,
    // so read it before the stop.
    open(app, &book_id, chapter_index, offset_ms, app.prefs.get().audio_rate);
}

/// Start the current chapter over from its beginning.
pub fn restart_from_beginning(app: &Rc<App>) {
    let Some(session) = app.abook.borrow().clone() else {
        return;
    };
    open(app, &session.book_id, session.chapter_index, 0, app.prefs.get().audio_rate);
}
