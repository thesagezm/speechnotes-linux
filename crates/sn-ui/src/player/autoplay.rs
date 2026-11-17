//! `--play-note` / `--speak` autoplay plumbing: the pending id handed over
//! the command line, served once the window exists. Split out of
//! `player.rs` (Round 4, pure code-motion).

use std::rc::Rc;

static AUTOPLAY: std::sync::OnceLock<String> = std::sync::OnceLock::new();


use crate::app::App;

use super::session::play_current;
use super::transport::set_status;

pub fn set_autoplay(prefix: &str) {
    let _ = AUTOPLAY.set(prefix.to_string());
}

pub(crate) fn take_autoplay() -> Option<String> {
    AUTOPLAY.get().cloned()
}

/// Serve a pending `--play-note` at the end of window construction.
pub fn serve_autoplay(app: &Rc<App>) {
    if let Some(prefix) = take_autoplay() {
        autoplay(app, &prefix);
    }
}

/// Auto-play a note by id prefix (`--play-note`), for the batch gate and for
/// scripting the app from a shell.
pub fn autoplay(app: &Rc<App>, id_prefix: &str) {
    let note = app
        .notes
        .active()
        .into_iter()
        .find(|n| n.id.starts_with(id_prefix));
    match note {
        Some(note) => {
            crate::window::open_note(app, &note.id);
            play_current(app);
        }
        None => set_status(app, &format!("No note matches {id_prefix:?}")),
    }
}

/// Play was asked for on an entry with nothing to say — a scanned page, an
/// image-only chapter. The first entry after it with text is found on a
/// worker (a cold page's recognition costs a second; the UI thread does not
/// wait on it) and the voice starts there.
// Carved original: the two functions below and the walk are verbatim from
// the player original 7e7836a6b903 (no-timestamp content blob); the final
// player.rs's copy of them did not survive anywhere else.
pub fn speak_from_first_speakable(book_id: String, start: usize) {
    crate::tasks::spawn(move || {
        let index = walk_to_speakable(&book_id, start);
        crate::tasks::Msg::SpeakableFound { book_id, index }
    });
}

/// The walk's worker half: the first entry at or after `start + 1` whose
/// text is speakable, within twenty entries. The book handle is this
/// thread's own — a `Book` cannot cross threads — and the parse memo plus
/// the OCR cache make the walk a cache read wherever the warm-up has passed.
fn walk_to_speakable(book_id: &str, start: usize) -> Option<usize> {
    let Ok(Some(book)) = sn_store::books::BooksStore::find(book_id) else {
        return None;
    };
    let last = book.chapters().saturating_sub(1);
    for index in (start + 1)..=(start + 20).min(last) {
        let Ok(text) = book.chapter_text(index) else {
            continue;
        };
        if sn_format::sanitizer::is_speakable(&text) {
            return Some(index);
        }
    }
    None
}

/// The walk's answer, on the UI thread: open the entry found and let the
/// voice start there; a walk that found nothing says so and stops.
//  the original cuts mid-doc-comment here; the body is authored from the
// lost fragment's own contract and the sibling advance_to_next_speakable_*
// walks' conventions (stale-bookmark removal, open, view follows, speak).
pub fn on_first_speakable_found(app: &Rc<App>, book_id: &str, index: Option<usize>) {
    let Some(index) = index else {
        // Twenty silent entries in a row: the voice stops here rather than
        // skipping past something the reader may want.
        crate::reader::set_status(app, "No page with text found — the voice stops here.");
        return;
    };
    // The walk enters the entry at its top: a stale bookmark from an old
    // listen must not catch the walk mid-entry.
    app.bookmarks
        .remove(&sn_store::model::book_key(book_id, index));
    let gen = crate::reader::open_book(app, book_id, index);
    // Stamped with this open's generation: the open that lands on it is the
    // one that speaks.
    app.reader.borrow_mut().speak_when_opened = Some(gen);
    // The view follows the voice: the page shows now, its text lands with
    // the open worker.
    if app.reader.borrow().format == "pdf" {
        if let Some(view) = crate::reader::pdf_view(app) {
            view.scroll_to_page(index);
        }
    }
}
