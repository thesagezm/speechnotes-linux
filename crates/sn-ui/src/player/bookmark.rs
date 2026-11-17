//! Bookmark persistence for the note player: where a note or a book was
//! last heard. Split out of `player.rs` (Round 4, pure code-motion).

use std::rc::Rc;

use crate::app::App;

pub(crate) fn save_bookmark(app: &Rc<App>, progress: &sn_tts::Progress) {
    if app.tts_saved_frontier.get() == progress.frontier {
        return;
    }
    app.tts_saved_frontier.set(progress.frontier);
    put_bookmark(app, progress, false);
}

pub(crate) fn save_bookmark_now(app: &Rc<App>, progress: &sn_tts::Progress) {
    put_bookmark(app, progress, true);
}

pub(crate) fn put_bookmark(app: &Rc<App>, progress: &sn_tts::Progress, now: bool) {
    let Some(id) = app.tts_note.borrow().clone() else {
        return;
    };
    let text = app.tts_text.borrow().clone();

    // A book chapter's position: keyed `book:<id>:<chapter>`, the shape the
    // iOS app's bookmarks.json uses. The fraction comes from the frontier
    // the same way a note's byte offset does — the store receives it, it
    // never feeds one back.
    if app.tts_book.get() {
        // The voice's section, not the view's: a relocation reports the
        // section the visible page starts in, and on a continuously-paginated
        // book the walk's section can share that page — the bookmark must
        // stay where the voice is, or resume lands pages (or sections) off.
        let chapter = app
            .reader
            .borrow()
            .walk_section
            .unwrap_or_else(|| app.reader.borrow().chapter);
        let bookmark = sn_store::model::PlaybackBookmark::for_note(
            &format!("book:{id}:{chapter}"),
            sn_store::model::utf16_offset(&text, progress.frontier),
            sn_core::util::stable_hash(&text),
        );
        if now {
            app.bookmarks
                .put_now(sn_store::model::book_key(&id, chapter), bookmark);
        } else {
            app.bookmarks
                .put(sn_store::model::book_key(&id, chapter), bookmark);
        }
        crate::reader::save_position(app, progress.frontier);
        return;
    }

    // A purged (or binned) note must not grow a fresh bookmark every second:
    // the purge removed it deliberately, and the session's tick would
    // otherwise resurrect it for as long as playback keeps running.
    match app.notes.get(&id) {
        Some(note) if !note.is_deleted() => {}
        _ => return,
    }
    if now {
        app.bookmarks.put_note_frontier_now(&id, &text, progress.frontier);
    } else {
        app.bookmarks.put_note_frontier(&id, &text, progress.frontier);
    }
}
