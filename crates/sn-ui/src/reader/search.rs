//! Split out of reader.rs 2026-10-31 (pure code-motion).

use std::rc::Rc;
use gtk4::glib;

use crate::app::App;
use super::*;

/// In-book search: every chapter containing `needle`, with the byte offset
/// of the first hit so a result can jump there.
///
/// This is the **synchronous** form — it walks the book on the calling
/// thread. The interactive path is [`search_current_book`], which debounces
/// and moves the walk to a worker; this one stays for callers that want the
/// answer directly (a test, a command-line path).
pub fn search(app: &Rc<App>, needle: &str) -> Vec<(usize, String, usize)> {
    let Ok(Some(book)) = current_book(app) else {
        return Vec::new();
    };
    search_in(&book, needle)
}

/// Search the open book and report. The chapter list is the expensive part
/// (one chapter-text extraction per chapter), so the status line says how
/// many chapters matched; the first match's chapter is the jump target.
///
/// **Debounced and off the UI thread.** The walk used to run over every
/// chapter on every keystroke, on the UI thread — seconds per keystroke in a
/// 1295-page book. Now the keystroke arms a 250 ms timer, the timer hands
/// the book to a worker, and the answer comes back tagged with the
/// generation it was asked for; a query typed while an older one was still
/// running wins, and the older answer is dropped.
pub fn search_current_book(app: &Rc<App>, needle: &str) {
    // A new query invalidates the last one's jump target: without this,
    // searching "witch", jumping, then clearing the query and pressing Enter
    // re-jumps to a chapter nothing matched.
    {
        let mut state = app.reader.borrow_mut();
        state.search_first_hit = None;
        // Every new query bumps the counter, so an answer for an older
        // query cannot win.
        state.search_generation += 1;
    }
    if needle.trim().is_empty() {
        // An empty query cancels any search still running.
        return;
    }
    // One timer per burst: a fast typist gets one search, not one per key.
    if let Some(old) = app.search_source.borrow_mut().take() {
        old.remove();
    }
    let app2 = app.clone();
    let needle = needle.to_string();
    let source = glib::timeout_add_local(
        std::time::Duration::from_millis(250),
        move || {
            *app2.search_source.borrow_mut() = None;
            run_search(&app2, &needle);
            glib::ControlFlow::Break
        },
    );
    *app.search_source.borrow_mut() = Some(source);
}

/// The search itself, on a worker: the walk, the fold, the offset mapping.
pub(crate) fn run_search(app: &Rc<App>, needle: &str) {
    let Ok(Some(book)) = current_book(app) else {
        return;
    };
    let gen = app.reader.borrow().search_generation;
    let manifest = book.manifest.clone();
    let needle = needle.to_string();
    crate::tasks::spawn(move || {
        // The `Book` handle holds `Rc`s and cannot cross a thread, so the
        // worker rebuilds its own from the manifest — and the archive parse
        // it needs is memoised process-wide, so this is a lookup, not a
        // re-read.
        let book = sn_store::books::BooksStore::from_manifest(manifest);
        let hits = search_in(&book, &needle);
        crate::tasks::Msg::SearchDone { gen, needle, hits }
    });
}

/// The walk over a book's chapters, off the UI thread.
pub(crate) fn search_in(book: &sn_store::books::Book, needle: &str) -> Vec<(usize, String, usize)> {
    if needle.trim().is_empty() {
        return Vec::new();
    }
    let folded = sn_core::util::fold(needle);
    let mut hits = Vec::new();
    for index in 0..book.chapters() {
        let Ok(text) = book.chapter_text(index) else {
            continue;
        };
        let Some(folded_offset) = sn_core::util::fold(&text).find(&folded) else {
            continue;
        };
        let heading =
            toc_label_for(book, index).unwrap_or_else(|| format!("Chapter {}", index + 1));
        // The offset is reported in the chapter's own byte domain — the
        // domain a bookmark or a scroll target would use. `fold` can change
        // byte length (NFKD decomposition), so the folded offset cannot be
        // used directly: walk the original text in lockstep with the folded
        // one until the folded cursor arrives, and report where that is.
        let original_offset = original_offset_of(&text, folded_offset);
        hits.push((index, heading, original_offset));
        if hits.len() >= 50 {
            break;
        }
    }
    hits
}

/// A search finished. Only the newest query's answer is applied.
pub fn on_search_done(
    app: &Rc<App>,
    gen: u64,
    needle: &str,
    hits: Vec<(usize, String, usize)>,
) {
    {
        let state = app.reader.borrow();
        if state.search_generation != gen {
            return;
        }
    }
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    if hits.is_empty() {
        w.reader
            .status
            .set_text(&format!("no match for {needle:?} in this book"));
        return;
    }
    let (chapter, heading, _) = &hits[0];
    w.reader.status.set_text(&format!(
        "{} match(es) — first in {}",
        hits.len(),
        heading
    ));
    // Enter jumps to the first hit: the search entry's activate signal is
    // wired in window.rs to the same first-hit chapter.
    app.reader.borrow_mut().search_first_hit = Some(*chapter);
}

/// Map a byte offset in a folded string back to the original's byte domain.
///
/// `fold` is NFKD + casefold, which can change a character's byte length, so
/// the two offsets are not the same number. Walking both in lockstep by
/// character is exact and cheap (a chapter is a few hundred KB at most) —
/// and it allocates nothing per character, which the old version did.
pub(crate) fn original_offset_of(original: &str, folded_offset: usize) -> usize {
    let mut folded_cursor = 0usize;
    for (byte, ch) in original.char_indices() {
        if folded_cursor >= folded_offset {
            return byte;
        }
        folded_cursor += sn_core::util::fold(ch.encode_utf8(&mut [0u8; 4])).len();
    }
    original.len()
}
