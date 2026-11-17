//! The audiobook player: a decoded stream, its transport, and the position
//! that survives a restart.
//!
//! The design is the mini-player's, one level up. The book page owns:
//!
//! * **The session** — an `AudiobookSession` (the ffmpeg pipe plus its writer
//!   thread) driving the sink directly. Position is *played frames*, the same
//!   write-ahead rule the notes player follows.
//! * **The tick** — a 250 ms glib timeout (slower than the TTS player's
//!   100 ms: a chapter is minutes long, not sentences) that writes the
//!   bookmark from the session's file position, updates the transport
//!   labels, and advances to the next chapter when the stream ends.
//! * **The bookmark** — `book:<id>:<chapter>` with a **millisecond**
//!   offset, the unit the iOS app's bookmarks.json uses for audiobooks
//!   (the same field, the key carrying the unit distinction — see
//!   `DATA-FORMAT.md`).
//! * **The sleep timer** — a minutes-until-stop countdown; at zero the
//!   session stops and the bookmark is left where the listener dozed off.
//!
//! A seek is a respawn: the pipe's start offset is fixed at spawn, so
//! seeking stops the session and opens a new one. That is the honest shape
//! of seeking with a child process — exact, and costing one spawn.

use std::rc::Rc;
use std::time::Duration;

mod open;
mod playback;
mod progress;
mod seek;
mod slider;

// Re-exported wholesale so every `crate::abook::*` path keeps resolving;
// glob re-exports preserve each item's own visibility.
pub use open::*;
pub use playback::*;
pub use seek::*;
pub use slider::*;

//  the original uses this const at its only remaining call site but the
// line that defined it did not survive; the value below (not pinned by any
// evidence) is the coalescing window the surrounding code implies — a
// bookmark save is throttled, not debounced, and the tick is 1 Hz.
const ABBOOK_SAVE_MIN_INTERVAL: Duration = Duration::from_secs(2);

use crate::app::App;

/// The `--play-audiobook` directive, set by the binary before the window
/// builds.
static PENDING_OPEN: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Record a book-id prefix to play once the window exists.
pub fn set_pending_open(prefix: &str) {
    let _ = PENDING_OPEN.set(prefix.to_string());
}

/// Serve a pending `--play-audiobook` at the end of window construction. The
/// resume point is the manifest's chapter plus the bookmark's millisecond
/// offset — the same pair the reader's own resume path uses, so both entry
/// points land in the same place.
pub fn serve_pending_open(app: &Rc<App>) {
    let Some(prefix) = PENDING_OPEN.get() else {
        return;
    };
    let Ok(Some(book)) = sn_store::books::BooksStore::find(prefix) else {
        return;
    };
    let (chapter, offset_ms) = resume_point(app, &book);
    let rate = app.prefs.get().audio_rate;
    open(app, &book.manifest.id, chapter, offset_ms, rate);
}

/// The live session, shared by the UI's closures: an `Rc` around the sn-tts
/// session plus the book it plays. Cloning a handle clones the `Rc`, not the

/// writer thread.
pub type AbookSession = std::rc::Rc<AbookHandle>;

pub struct AbookHandle {
    pub inner: sn_tts::audiobook::AudiobookSession,
    /// The book's id and the chapter now playing.
    pub book_id: String,
    pub chapter_index: usize,
    /// The offset the session was opened with, in milliseconds into the
    /// chapter. The pipe's own position is zero at spawn, so the first ticks
    /// would otherwise report the chapter's start as the resume point and
    /// overwrite a bookmark the listener had already earned.
    offset_ms: u64,
}

impl Drop for AbookHandle {
    fn drop(&mut self) {
        self.inner.stop();
    }
}

impl AbookHandle {
    /// Milliseconds into the current chapter: the greater of what was asked
    /// for and what the pipe has actually played.
    pub fn offset_ms(&self, chapter_start: f64) -> u64 {
        let played = ((self.inner.file_seconds() - chapter_start).max(0.0) * 1000.0) as u64;
        played.max(self.offset_ms)
    }
}

/// The chapter and millisecond offset a book resumes from.
///
/// The bookmark is the source: its key names the chapter and its offset is
/// in milliseconds. The manifest's `chapterFraction` is whole-book progress,
/// a different number for a different reader.
pub fn resume_point(
    app: &Rc<App>,
    book: &sn_store::books::Book,
) -> (usize, u64) {
    let chapters = &book.manifest.chapters;
    let chapter = book.manifest.position.chapter_index.min(chapters.len().saturating_sub(1));
    // The bookmark's offset is in milliseconds; the manifest's fraction is
    // whole-book progress, so the bookmark is the resume source and the
    // fraction is the shelf's.
    let offset_ms = app
        .bookmarks
        .for_book_chapter(&book.manifest.id, chapter)
        .map(|b| b.text_offset.max(0) as u64)
        .unwrap_or(0);
    (chapter, offset_ms)
}

/// Whether an audiobook session is live.
///
/// A plain bool from a borrow that is dead by the time the caller acts —
/// the pattern every transport handler shares, kept here so no call site
/// holds the RefCell across a call that can re-enter `open`/`close`. A live
/// `borrow()` under a `borrow_mut()` is the "RefCell already borrowed"
/// panic, and one such panic was seen taking the window down on close.
pub fn is_live(app: &Rc<App>) -> bool {
    app.abook.borrow().is_some()
}
