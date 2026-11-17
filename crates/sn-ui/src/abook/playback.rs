//! Playback: the resume paths and the 250 ms tick that advances chapters
//! and writes bookmarks. Split out of `abook.rs` (Round 4, pure
//! code-motion).

use std::rc::Rc;

use gtk4::glib;
use gtk4::prelude::*;
use std::time::Duration;

use crate::app::App;

use super::open::{close, open, toggle};
use super::{is_live, resume_point};
use super::progress::{clock, save_position, whole_fraction};

pub fn maybe_toggle(app: &Rc<App>) -> bool {
    if is_live(app) {
        toggle(app);
        return true;
    }
    resume_current(app)
}

/// Re-open the audiobook the reader pane is showing, from its bookmark.
///
/// The reader-page check matters: with a stopped audiobook's display still
/// up, the transport's play button and F5 must bring *the book* back, not
/// start speaking a note the listener is not looking at. But when the editor
/// stack has moved on to a note, the same buttons belong to the note player.
pub(crate) fn resume_current(app: &Rc<App>) -> bool {
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
    let Some(id) = app.reader.borrow().book_id.clone() else {
        return false;
    };
    let Ok(Some(book)) = sn_store::books::BooksStore::find(&id) else {
        return false;
    };
    if !book.is_audiobook() {
        return false;
    }
    let (chapter, offset_ms) = resume_point(app, &book);
    open(app, &book.manifest.id, chapter, offset_ms, app.prefs.get().audio_rate);
    true
}

/// The 250 ms poll: position label, bookmark, auto-advance, sleep timer.
///
/// The sleep timer is the pref `sleepTimerMinutes`, 0 meaning off. It counts
/// down while a session plays and stops it at zero, leaving the bookmark
/// where the listener dozed off — the whole point of a sleep timer, and the
/// reason the bookmark is written every tick rather than on pause.
pub(crate) fn attach_tick(app: &Rc<App>) {
    if let Some(old) = app.abook_tick.borrow_mut().take() {
        old.remove();
    }
    let tick_app = app.clone();
    let source = glib::timeout_add_local(Duration::from_millis(250), move || {
        tick(&tick_app)
    });
    *app.abook_tick.borrow_mut() = Some(source);
    // The countdown is kept in the App beside the source that reads it, so
    // one source owns it and a new session starts from the pref again.
    let minutes = app.prefs.get().sleep_timer_minutes;
    *app.abook_sleep.borrow_mut() = if minutes == 0 {
        None
    } else {
        Some(std::time::Instant::now() + Duration::from_secs(minutes as u64 * 60))
    };
}

pub(crate) fn tick(app: &Rc<App>) -> glib::ControlFlow {
    // Clone the session out and drop the borrow before anything can call
    // `open`/`close`: the borrow must not be held across a re-entrant call.
    let Some(session) = app.abook.borrow().clone() else {
        return glib::ControlFlow::Break;
    };
    let Ok(Some(book)) = sn_store::books::BooksStore::find(&session.book_id) else {
        return glib::ControlFlow::Break;
    };
    // A decode failure is not a chapter ending. The old pipe swallowed
    // ffmpeg's stderr, so a corrupt file read as EOF and the UI skipped to
    // the next chapter as if the listener had asked — one silent skip per
    // broken file, all the way to the end of the book. Named here, once,
    // and the session stops where it is. No final bookmark is written on
    // this path on purpose: the stream broke, and the last honest tick's
    // position (≤250 ms stale) is the right resume point for a retry.
    if let Some(why) = session.inner.take_error() {
        close(app);
        if let Some(w) = app.widgets.borrow().as_ref() {
            w.reader.status.set_visible(true);
            w.reader.abook_position.set_visible(false);
            w.reader.speak.set_play();
            w.reader.status.set_text(&format!("Audio failed: {why}"));
        }
        return glib::ControlFlow::Break;
    }
    let chapter = book.manifest.chapters.get(session.chapter_index);
    let chapter_start = chapter.map(|c| c.start_seconds).unwrap_or(0.0);

    // The position label: elapsed in the chapter, and the chapter's length.
    let elapsed = session.offset_ms(chapter_start) as f64 / 1000.0;
    let length = chapter.map(|c| c.end_seconds - c.start_seconds).unwrap_or(0.0);
    let (elapsed_text, length_text) = (clock(elapsed), clock(length));
    // The transport bar's time is whole-book, matching its fill: the
    // chapter readout lives on the book page, above the chapter bar; the
    // bottom bar counts through the entire book. The chapter's absolute
    // start plus the elapsed-in-chapter is the file position — the same
    // number the ffmpeg pipe is playing.
    let whole_elapsed = chapter_start + elapsed;
    let whole_total = book
        .manifest
        .chapters
        .last()
        .map(|c| c.end_seconds)
        .unwrap_or(0.0);
    let (whole_text, whole_total_text) = (clock(whole_elapsed), clock(whole_total));
    if let Some(w) = app.widgets.borrow().as_ref() {
        w.reader
            .abook_position
            .set_text(&format!("{elapsed_text} / {length_text}"));
        // The book display's own readout moves with the transport's.
        let abook = &w.reader.abook;
        abook
            .position
            .set_text(&format!("{elapsed_text} / {length_text}"));
        // The chapter-scoped bar: how far into *this chapter* the listener
        // is. The whole-book fraction lives in the transport bar. A scrub
        // gesture holds this write, so the drag paints its own target.
        if !app.abook_scrub.get() {
            abook.chapter_bar.set_fraction(if length > 0.0 {
                (elapsed / length).clamp(0.0, 1.0)
            } else {
                0.0
            });
        }
        // The bar is live while the book plays: the fill carries the accent
        // glow, added and removed by this tick — no new timer.
        let playing = !session.inner.is_paused();
        if playing && !abook.chapter_bar.has_css_class("live") {
            abook.chapter_bar.add_css_class("live");
        } else if !playing && abook.chapter_bar.has_css_class("live") {
            abook.chapter_bar.remove_css_class("live");
        }
        // The play button follows the paused state; the book display's
        // belongs to the same session.
        abook.play.set_playing(playing);
        // The chapter's name, so turning dials is not the only way to see
        // what is playing.
        abook.chapter.set_text(chapter.map(|c| c.title.as_str()).unwrap_or(""));
    }
    crate::player::describe_audiobook(
        app,
        &book.manifest.title,
        chapter.map(|c| c.title.as_str()).unwrap_or(""),
        &whole_text,
        &whole_total_text,
        whole_fraction(&book, session.chapter_index, elapsed),
    );

    // The bookmark: milliseconds into the chapter, the iOS unit. The book is
    // the one this tick already found — the old code called `find` again
    // here, a second full walk of every manifest, four times a second.
    save_position(app, &book);

    // The sleep timer: at zero the session stops and the bookmark stays
    // where the listener stopped listening.
    if let Some(deadline) = *app.abook_sleep.borrow() {
        if std::time::Instant::now() >= deadline {
            close(app);
            if let Some(w) = app.widgets.borrow().as_ref() {
                w.reader.status.set_visible(true);
                w.reader.status.set_text("Sleep timer stopped playback");
                w.reader.abook_position.set_visible(false);
                w.reader.speak.set_play();
            }
            return glib::ControlFlow::Break;
        }
    }

    // Auto-advance at the chapter's end. The pipe is spawned at the
    // chapter's start offset with no duration limit, so for a continuous
    // file it decodes straight through the boundary into the next chapter's
    // audio — `is_finished` fires only at the end of the *book*. The
    // playhead crossing the manifest's chapter end is the real per-chapter
    // end: without this check the UI sat on "17:06 / 10:09" while chapter
    // six's audio played, and the tick's bookmark quietly grew past the
    // chapter it was keyed to. The overshoot (≤ one 250 ms tick) is
    // discarded; the next chapter starts at its top. `is_finished` stays
    // for a file that ends inside the last chapter. A zero-length chapter
    // never advances — that would be a walk, not a listen.
    let chapter_over = length > 0.0 && elapsed >= length;
    if (session.inner.is_finished() || chapter_over) && !session.inner.is_paused() {
        let next = session.chapter_index + 1;
        if next < book.manifest.chapters.len() {
            open(app, &book.manifest.id, next, 0, app.prefs.get().audio_rate);
        } else {
            close(app);
            if let Some(w) = app.widgets.borrow().as_ref() {
                w.reader.status.set_visible(true);
                w.reader.status.set_text("Finished");
                w.reader.abook_position.set_visible(false);
                w.reader.speak.set_play();
            }
            return glib::ControlFlow::Break;
        }
    }
    glib::ControlFlow::Continue
}
