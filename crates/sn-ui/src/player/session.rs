//! The note player's session lifecycle: toggling, starting (from a note,
//! an offset, or a book page), stopping, and the 100 ms tick that follows
//! the engine's progress. Split out of `player.rs` (Round 4, pure
//! code-motion).

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gtk4::glib;

use crate::app::App;

use super::bookmark::{save_bookmark, save_bookmark_now};
use super::highlight::{update_read_along, update_widgets};
use super::transport::{detach, set_play_icon, set_status, show_bar};
use super::TICK_MS;

pub fn toggle_play(app: &Rc<App>) {
    let session = app.tts.borrow().clone();
    let Some(session) = session else {
        play_current(app);
        return;
    };
    match session.progress().state {
        sn_tts::PlayState::Playing => session.pause(),
        sn_tts::PlayState::Paused => {
            if let Err(e) = session.resume() {
                set_status(app, &format!("Resume failed: {e}"));
            }
        }
        sn_tts::PlayState::Finished | sn_tts::PlayState::Stopped | sn_tts::PlayState::Failed => {
            play_current(app)
        }
    }
}

/// Play the open note, resuming from its bookmark when one is still valid.
///
/// A book on the reader page owns the bar first: the transport is the app's,
/// and play on a book page means the book — from the page on screen, the
/// same thing F5 does — never the note sitting behind the reader. A session
/// that is playing or paused never reaches here (toggle pauses or resumes);
/// a finished one does, and re-speaks the book from where the reader stands.
pub fn play_current(app: &Rc<App>) {
    if crate::reader::speak_open_book(app) {
        return;
    }
    play_note_at(app, None);
}

/// Play the open note from a character offset into its text — the editor's
/// "Speak from here". The bookmark is not consulted: the offset *is* the
/// resume point, and the tick's write-ahead takes over from the first
/// sentence onward, so the position survives as any other would.
pub fn play_note_from(app: &Rc<App>, char_offset: usize) {
    play_note_at(app, Some(char_offset));
}

pub(crate) fn play_note_at(app: &Rc<App>, from_char: Option<usize>) {
    crate::window::commit_pending_edit(app);
    let Some(note) = app.open_note() else {
        return;
    };
    let text = note.text.clone();
    if !sn_format::sanitizer::is_speakable(&text) {
        set_status(app, "Nothing in this note can be spoken.");
        show_bar(app);
        return;
    }
    // A character offset from a GTK iterator maps into the note's own text:
    // the buffer the offset came from *is* this text (commit_pending_edit
    // writes it through unchanged). A bookmark resume is only ever *read*
    // here: it seeds the byte, and the session's own frontier takes over
    // from the first tick. The store converts the file's UTF-16 offset to
    // our byte domain — the one place that conversion lives (see
    // DATA-FORMAT.md).
    let resume = match from_char {
        Some(chars) => text
            .char_indices()
            .nth(chars)
            .map(|(b, _)| b)
            .unwrap_or(text.len()),
        None => app.bookmarks.note_resume_byte(&note.id, &text).unwrap_or(0),
    };
    if from_char.is_some() && !sn_format::sanitizer::is_speakable(&text[resume..]) {
        set_status(app, "Nothing left to speak from there.");
        return;
    }
    if let Err(e) = start_session(app, note.id.clone(), text, resume) {
        set_status(app, &e);
        show_bar(app);
    }
}

/// Stop: the tick finalises the bookmark from the session's frontier.
pub fn stop_playback(app: &Rc<App>) {
    if let Some(session) = app.tts.borrow().as_ref() {
        session.stop();
    }
}

/// Skip back to the very beginning: forget the bookmark and start over.
pub fn restart_from_beginning(app: &Rc<App>) {
    let playing_id = app.tts_note.borrow().clone();
    if let Some(id) = &playing_id {
        app.bookmarks.remove_note(id);
    }
    if app.tts.borrow().is_some() {
        if let Some(session) = app.tts.borrow().as_ref() {
            session.stop();
        }
        app.tts_restart.set(Some(0));
    } else {
        app.tts_note.borrow_mut().take();
        play_current(app);
    }
}

/// Window shutdown: one final bookmark from the frontier, then stop.
pub fn shutdown(app: &Rc<App>) {
    if let Some(session) = app.tts.borrow().as_ref() {
        save_bookmark_now(app, &session.progress());
        session.stop();
    }
}

/// Start a session over arbitrary text — the reader's per-chapter speech.
/// The id is the book's, and the bookmark key is per chapter, so a chapter's
/// position survives a session stop exactly as a note's does.
/// The reader's per-chapter speech. The book flag is set inside the load
/// completion (start_session's background load may fail — setting it here
/// would leave the tick writing book bookmarks for a session that never
/// started).
pub fn start_session_for_text(
    app: &Rc<App>,
    id: String,
    text: String,
    resume_byte: usize,
) -> Result<(), String> {
    start_session_inner(app, id, text, resume_byte, true)
}

pub(crate) fn start_session(
    app: &Rc<App>,
    note_id: String,
    text: String,
    resume_byte: usize,
) -> Result<(), String> {
    start_session_inner(app, note_id, text, resume_byte, false)
}

/// `is_book` decides which bookmark key the tick writes once the session is
/// live; it is captured by the load-completion path and set on the App
/// there, so it never claims a domain for a session that failed to start.
pub(crate) fn start_session_inner(
    app: &Rc<App>,
    note_id: String,
    text: String,
    resume_byte: usize,
    is_book: bool,
) -> Result<(), String> {
    // Any previous session is stopped and abandoned; its bookmark was
    // already written by its tick (or by `shutdown`), and its threads exit
    // on the stop flag without the UI waiting on them.
    if let Some(old) = app.tts.borrow().as_ref() {
        old.stop();
    }

    // An open audiobook owns a sink to the same device this session is about
    // to open, and nothing in the audio layer serialises the two: two live
    // writers means interleaved audio and independent underruns. The reader's
    // speak button refuses to run on an open book (it dispatches to
    // `abook::toggle`), but the transport bar, a keyboard shortcut, and a
    // queued restart all reach here without that check. Closing the book here
    // is the mirror of `abook::open` stopping the TTS session — one sound at
    // a time, decided by whichever the user asked for.
    crate::abook::close(app);

    // The engine comes from prefs — the same field the picker writes and
    // `--speak` reads — and through the App's model manager, whose
    // `resident()` is the single-resident discipline: asking for a
    // different engine drops the old one's model pool *before* the new
    // graph is opened, so two 300 MB pools are never live at once no matter
    // how quickly the picker or the speed control restarts playback.
    // The engine load happens on a background thread. A cold supertonic
    // load is ~3.4 s and kokoro ~2.4 s on this box; loading on the UI thread
    // froze the window for exactly that long on every engine or voice
    // change — the "regular freezing" the user reported. The session itself
    // still starts on the UI thread (it opens the sink); this closure is
    // marshalled back with glib::idle_add so the tick, not a signal handler,
    // owns the swap.
    let prefs = app.prefs.get();
    let engine_kind = prefs.engine_kind.clone();
    let voice = prefs.voice_for_engine(&engine_kind);
    let rate = prefs.rate_multiplier;
    let lang = prefs.speech_language.clone();
    // The UI half never blocks: a oneshot channel the worker sends the
    // loaded engine (or the error) into, polled by a short glib timeout that
    // was installed on the UI thread before the worker spawned. `Rc<App>`
    // never crosses the thread boundary — only the `Send` payload does.
    let (tx, rx) = std::sync::mpsc::channel::<Result<Arc<dyn sn_tts::Engine>, String>>();
    {
        let models = app.models.clone();
        std::thread::spawn(move || {
            let engine = {
                let mut manager = models.lock().unwrap_or_else(|e| e.into_inner());
                manager
                    .resident(&engine_kind, &voice, rate, &lang)
                    .map_err(|e| e.to_string())
            };
            // The receiver may be gone (the app quit); a failed send is not
            // an error worth reporting anywhere.
            let _ = tx.send(engine);
        });
    }
    {
        // Status now, so the press visibly did something during the load.
        show_bar(app);
        set_status(app, "Loading the voice…");
        let app = app.clone();
        let note_id = note_id.clone();
        let text = text.clone();
        let rx = std::cell::RefCell::new(rx);
        glib::timeout_add_local(Duration::from_millis(50), move || {
            match rx.borrow_mut().try_recv() {
                Ok(Ok(engine)) => {
                    app.tts_book.set(is_book);
                    if let Err(e) = start_session_with_engine(
                        &app, note_id.clone(), text.clone(), resume_byte, engine,
                    ) {
                        set_status(&app, &e);
                    }
                    glib::ControlFlow::Break
                }
                Ok(Err(e)) => {
                    set_status(&app, &e);
                    glib::ControlFlow::Break
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                // Disconnected: the worker panicked or the payload was
                // dropped; the status line says so rather than spinning.
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    set_status(&app, "The voice failed to load.");
                    glib::ControlFlow::Break
                }
            }
        });
    }
    Ok(())
}

/// The half of [`start_session`] that runs once the engine is loaded: open
/// the sink, spawn the pipeline, own the state. On the UI thread, because
/// GTK owns the widgets it updates.
#[allow(clippy::too_many_arguments)]
pub(crate) fn start_session_with_engine(
    app: &Rc<App>,
    note_id: String,
    text: String,
    resume_byte: usize,
    engine: Arc<dyn sn_tts::Engine>,
) -> Result<(), String> {
    if let Some(old) = app.tts.borrow().as_ref() {
        old.stop();
    }
    let session = sn_tts::SessionCore::start(
        &text,
        resume_byte,
        engine,
        sn_tts::SessionConfig::default(),
    )
    .map_err(|e| format!("Could not start playback: {e}"))?;

    let (epoch, plan) = session.plan();
    *app.tts_plan.borrow_mut() = (epoch, plan.chunks);
    *app.tts_note.borrow_mut() = Some(note_id.clone());
    *app.tts_text.borrow_mut() = text;
    app.tts_hash
        .set(sn_core::util::stable_hash(&app.tts_text.borrow()));
    app.tts_saved_frontier.set(usize::MAX);
    *app.tts.borrow_mut() = Some(session);

    show_bar(app);
    set_play_icon(app, false);
    // The voice owns the reader's page while it speaks: the view follows the
    // voice, and the reader's own scrolling does not move the manifest's
    // position out from under it.
    if app.tts_book.get() {
        crate::reader::set_speaking(app, true);
    }
    attach_tick(app);
    tracing::info!(note = %note_id, resume = resume_byte, "playback started");
    Ok(())
}

pub(crate) fn attach_tick(app: &Rc<App>) {
    if let Some(old) = app.tts_tick.borrow_mut().take() {
        old.remove();
    }
    let tick_app = app.clone();
    let source =
        glib::timeout_add_local(Duration::from_millis(TICK_MS), move || tick(&tick_app));
    *app.tts_tick.borrow_mut() = Some(source);
}

/// One poll of the session: bookmark, read-along, widgets, terminal states.
pub(crate) fn tick(app: &Rc<App>) -> glib::ControlFlow {
    let Some(session) = app.tts.borrow().clone() else {
        return glib::ControlFlow::Break;
    };
    let progress = session.progress();

    if progress.state == sn_tts::PlayState::Playing {
        save_bookmark(app, &progress);
    } else if progress.state.is_terminal() {
        if progress.state == sn_tts::PlayState::Finished {
            // Completeness is a signal: a finished chapter has no place left
            // to keep, so its bookmark goes rather than pointing at the end —
            // with the *book* key, or a 73-chapter book would fill the LRU
            // with stale end positions and evict live ones.
            let (id, is_book, chapter) = {
                let reader = app.reader.borrow();
                (
                    app.tts_note.borrow().clone(),
                    app.tts_book.get(),
                    reader.chapter,
                )
            };
            if let Some(id) = &id {
                if is_book {
                    app.bookmarks
                        .remove(&sn_store::model::book_key(id, chapter));
                } else {
                    app.bookmarks.remove_note(id);
                }
            }
            // The voice walks a book forward on its own: a PDF's pages, an
            // EPUB's chapters. The view follows it in both, and a chapter
            // with nothing to say is passed rather than ending the walk.
            if is_book {
                let format = app.reader.borrow().format.clone();
                if let Some(id) = &id {
                    match format.as_str() {
                        "pdf" => advance_to_next_speakable_page(app, id),
                        // An EPUB advances by chapter.
                        "epub" => advance_to_next_speakable_chapter(app, id),
                        // The engine-rendered documents walk on the engine's
                        // own sections: navigating the engine, and the
                        // engine's text decides what is speakable — the old
                        // parse's chapter list cannot steer a book the
                        // engine renders (its sections and its chapter
                        // counts do not even agree).
                        _ if crate::reader::engine_rendered(app) => {
                            advance_engine_voice(app, id)
                        }
                        other if sn_doc::bookdoc::DocFamily::is_document_extension(other) => {
                            advance_to_next_speakable_chapter(app, id)
                        }
                        // A deck's slide IS its chapter: the voice walks the
                        // deck the same way, and the deck's text layer is the
                        // cheap walk — no OCR stands between one slide and
                        // the next.
                        other if crate::reader::is_slides_format(other) => {
                            advance_to_next_speakable_slide(app, id)
                        }
                        _ => {}
                    }
                }
            }
        } else {
            save_bookmark_now(app, &progress);
        }
    }

    update_read_along(app, &progress);
    update_widgets(app, &progress);

    // A chunk the engine could not synthesise announces itself once, in
    // words, on the status line: the skip-beep says something was missed,
    // the line says why. Without it, a voice that fails on every chunk
    // (a model deleted under the app, a voice pref the engine cannot
    // honour) reads to the listener as "the app only beeps now" with
    // nothing on screen — exactly the report of 2026-11-13.
    if progress.errors > app.tts_errors_shown.get() {
        app.tts_errors_shown.set(progress.errors);
        if progress.state == sn_tts::PlayState::Playing {
            set_status(
                app,
                &format!(
                    "{} sentence(s) skipped — the voice failed to synthesise them",
                    progress.errors
                ),
            );
        }
    }

    // A speed change queued a restart at a frontier; the old session must be
    // fully terminal before a new sink is opened.
    if progress.state.is_terminal() {
        if let Some(frontier) = app.tts_restart.take() {
            let note_id = app.tts_note.borrow().clone();
            let text = app.tts_text.borrow().clone();
            *app.tts.borrow_mut() = None;
            if let Some(note_id) = note_id {
                if let Err(e) = start_session(app, note_id, text, frontier) {
                    set_status(app, &e);
                    detach(app);
                    return glib::ControlFlow::Break;
                }
                return glib::ControlFlow::Continue;
            }
        }
        detach(app);
        return glib::ControlFlow::Break;
    }
    glib::ControlFlow::Continue
}

/// The PDF's voice walks the pages: the next page with something to say is
/// opened, spoken, and the view follows it.
///
/// A scanned book has no speakable page at all, and a book with a figure in
/// the middle has a page the voice must pass. The walk looks five pages
/// ahead and stops when it finds nothing — the same cap
/// `first_speakable_chapter` uses, and for the same reason: proving that a
/// whole book is silent would mean extracting every page.
pub(crate) fn advance_to_next_speakable_page(app: &Rc<App>, book_id: &str) {
    let Ok(Some(book)) = crate::reader::current_book(app) else {
        return;
    };
    if book.manifest.id != book_id {
        return;
    }
    let start = app.reader.borrow().chapter;
    let last = book.chapters().saturating_sub(1);
    let mut next = None;
    for page in (start + 1)..=(start + 5).min(last) {
        let Ok(text) = book.chapter_text(page) else {
            continue;
        };
        if sn_format::sanitizer::is_speakable(&text) {
            next = Some(page);
            break;
        }
    }
    let Some(page) = next else {
        // The end of the book, or five silent pages in a row: the voice
        // stops here rather than skipping a chapter the reader may want.
        crate::reader::set_status(app, "End of the book.");
        return;
    };
    // The walk enters the page at its top: a stale bookmark from an old
    // listen must not catch the walk mid-page — the same grounds the
    // Finished arm removed the finished page's bookmark.
    app.bookmarks
        .remove(&sn_store::model::book_key(book_id, page));
    let gen = crate::reader::open_book(app, book_id, page);
    // The view follows the voice: the page is on screen before its text is
    // spoken.
    if let Some(view) = crate::reader::pdf_view(app) {
        view.scroll_to_page(page);
    }
    // The voice speaks the page **that just landed**, not the one that just
    // finished: until the open's worker lands, `state.text` still holds the
    // finished page, and speaking it here re-read the page the listener had
    // just heard while the view moved on. Stamped with this open's
    // generation, `on_book_opened` starts the speech the moment the new
    // page's own text arrives — once, from its top.
    app.reader.borrow_mut().speak_when_opened = Some(gen);
}

/// The deck's voice walks the slides: the next slide with something to say
/// is opened, spoken, and the view (and any running show) follows it.
///
/// The walk runs to the deck's END, not the chapter walk's ten-ahead cap:
/// a deck's slides are routinely picture-only, and the slide text layer
/// (`chapter_text`) costs no OCR, so proving the rest of the deck silent is
/// the cheap walk it already is — the ten-ahead cap would strand the voice
/// ten slides into an image-heavy deck. The speak-from bubble and the plan
/// still reach a picture's recognised text through `chapter_full` once the
/// slide is open.
pub(crate) fn advance_to_next_speakable_slide(app: &Rc<App>, book_id: &str) {
    let Ok(Some(book)) = crate::reader::current_book(app) else {
        return;
    };
    if book.manifest.id != book_id {
        return;
    }
    let start = app.reader.borrow().chapter;
    let last = book.chapters().saturating_sub(1);
    let mut next = None;
    for slide in (start + 1)..=last {
        let Ok(text) = book.chapter_text(slide) else {
            continue;
        };
        if sn_format::sanitizer::is_speakable(&text) {
            next = Some(slide);
            break;
        }
    }
    let Some(slide) = next else {
        crate::reader::set_status(app, "End of the deck.");
        return;
    };
    // The walk enters the slide at its top — a stale bookmark from an old
    // listen must not catch the walk mid-slide.
    app.bookmarks
        .remove(&sn_store::model::book_key(book_id, slide));
    let gen = crate::reader::open_book(app, book_id, slide);
    // The voice speaks the slide that just landed — see the chapter walk's
    // note: `state.text` is still the finished slide's until the open lands.
    app.reader.borrow_mut().speak_when_opened = Some(gen);
}

/// The EPUB's voice walks the chapters: the next chapter with something to
/// say is opened at its top, spoken, and the read-along view follows it.
///
/// The walk looks ten chapters ahead and stops when it finds nothing — the
/// PDF walk's five-page cap in the same spirit. An EPUB chapter with no
/// speakable text is rare (the format is prose), but an image-only chapter
/// is legal, and proving a whole book silent would mean extracting it whole.
pub(crate) fn advance_to_next_speakable_chapter(app: &Rc<App>, book_id: &str) {
    let Ok(Some(book)) = crate::reader::current_book(app) else {
        return;
    };
    if book.manifest.id != book_id {
        return;
    }
    let start = app.reader.borrow().chapter;
    let last = book.chapters().saturating_sub(1);
    let mut next = None;
    for chapter in (start + 1)..=(start + 10).min(last) {
        let Ok(text) = book.chapter_text(chapter) else {
            continue;
        };
        if sn_format::sanitizer::is_speakable(&text) {
            next = Some(chapter);
            break;
        }
    }
    let Some(chapter) = next else {
        crate::reader::set_status(app, "End of the book.");
        return;
    };
    // The walk enters the chapter at its top — a stale bookmark from an old
    // listen must not catch the walk mid-chapter.
    app.bookmarks
        .remove(&sn_store::model::book_key(book_id, chapter));
    let gen = crate::reader::open_book(app, book_id, chapter);
    // The voice speaks the chapter that just landed — see the PDF walk's
    // note: `state.text` is still the finished chapter's until the open
    // lands, and speaking it here re-read what the listener had just heard.
    app.reader.borrow_mut().speak_when_opened = Some(gen);
}

/// The engine-rendered documents' voice walk: the Mobipocket/Kindle family,
/// FB2, RTF, HTML, plain text. The next section is the engine's to open — a
/// navigation, never a re-open (a re-open would re-run the engine's parse) —
/// and the voice starts on that section's own text.
///
/// **The walk owns the progression; the relocation follows.** The engine's
/// relocation reports the section the visible *page* starts in, and a
/// continuously-paginated book can lay the next section onto the very page
/// the reader is on (the mobi's front matter is five one-line sections that
/// share one page; a chapter's tail and the next chapter's head can share
/// another). A walk that waited for the relocation's index to change would
/// stall exactly there — measured 2026-11-17: the voice finished the title
/// page, the walk steered `go_to_index(1)`, the page did not move, no
/// relocation with a new index ever came, and the book went silent. And a
/// walk that tracked `chapter` fared no better: the relocation answering the
/// walk's own steering reported the page start and dragged the chapter back,
/// re-reading the same section forever. So the walk keeps its own position
/// (`walk_section`) and the section's *cached text* decides (the shell keeps
/// every loaded section's prose): speakable → the voice starts now; empty →
/// the section still loads and its report decides (the flag is already aimed
/// at it); present but unspeakable → the walk passes it by. The navigation
/// rides along so the view lands where the voice is.
pub(crate) fn advance_engine_voice(app: &Rc<App>, book_id: &str) {
    let Ok(Some(book)) = crate::reader::current_book(app) else {
        return;
    };
    if book.manifest.id != book_id {
        return;
    }
    let start = app.reader.borrow().walk_section.unwrap_or_else(|| {
        app.reader.borrow().chapter
    });
    // The spine count is the engine's own section count — the book report
    // wrote it into the manifest when the engine opened.
    let count = book.chapters();
    if start + 1 >= count {
        app.reader.borrow_mut().walk_section = None;
        crate::reader::set_status(app, "End of the book.");
        return;
    }
    if app.reader.borrow().voice_walk_skips >= 20 {
        // Twenty sections in a row without words: the voice stops here
        // rather than skipping past something the reader may want. (The
        // count is what the old walks' ten-ahead caps were for; the
        // engine's text is cheap, but a whole silent book is still not a
        // thing to prove one section at a time.)
        let mut state = app.reader.borrow_mut();
        state.voice_walk_skips = 0;
        state.walk_section = None;
        drop(state);
        crate::reader::set_status(app, "No page with text found — the voice stops here.");
        return;
    }
    {
        let mut state = app.reader.borrow_mut();
        state.voice_walk_skips += 1;
        // The walk's own position moves now — every later relocation is the
        // view's business, never the walk's.
        state.walk_section = Some(start + 1);
    }
    // The walk enters the section at its top — a stale bookmark from an old
    // listen must not catch the walk mid-section.
    app.bookmarks
        .remove(&sn_store::model::book_key(book_id, start + 1));
    // The flag stays aimed at the section for the one path that needs it —
    // the fetch answering empty, whose load report then decides. The fetch's
    // own answers take it and decide in place.
    {
        let gen = app.reader.borrow().generation;
        app.reader.borrow_mut().speak_when_opened = Some(gen);
    }
    // The walk steers, then asks for the section's text. The answer lands on
    // the UI thread a moment later; the view's navigation is already under
    // way. A user page turn racing the answer moves `walk_section` out of
    // the way (the follow flow clears it) — the walk stands down and the
    // section's own report (or the next Finished) takes over.
    if let Some(w) = app.widgets.borrow().clone() {
        let callback_app = app.clone();
        let callback_book = book_id.to_string();
        w.reader.book.section_text(start + 1, move |index, loaded, text| {
            walk_section_text(&callback_app, &callback_book, index, loaded, text);
        });
        w.reader.book.go_to_index(start + 1);
    }
}

/// The walk's answer to "what does the next section say". `index` is the
/// section the walk steered into; a `walk_section` that has moved off it
/// means the reader (or the follow flow) redirected the voice while the
/// fetch was in flight — the walk stands down. `loaded` false means the
/// section has not rendered yet: the steering loads it, and its load report
/// (which the walk's own gate accepts) decides then.
fn walk_section_text(
    app: &Rc<App>,
    book_id: &str,
    index: usize,
    loaded: bool,
    text: String,
) {
    let speak_now = {
        let mut state = app.reader.borrow_mut();
        if state.walk_section != Some(index) {
            return;
        }
        if loaded {
            state.text = text.clone();
        }
        // The decided answers consume the flag here; the still-loading one
        // leaves it aimed at the section's own load report.
        if loaded {
            state.speak_when_opened.take().is_some()
        } else {
            false
        }
    };
    if !loaded {
        // Not rendered yet — the navigation loads it, and its report
        // consumes the flag: speak it or pass it by, back into the walk.
        tracing::debug!(index, "walk: section text not loaded yet");
        return;
    }
    if !speak_now {
        // The section's own report already decided this section (its load
        // raced the fetch ahead of the answer); the voice is where it should
        // be either way.
        return;
    }
    if sn_format::sanitizer::is_speakable(&text) {
        tracing::debug!(index, len = text.len(), "walk: speaking section");
        {
            let mut state = app.reader.borrow_mut();
            state.voice_walk_skips = 0;
        }
        // The section top, unconditionally — the walk enters at the top even
        // when the section shares the page the reader is looking at.
        crate::reader::speak_chapter_from(app, Some(0));
    } else {
        tracing::debug!(index, len = text.len(), "walk: section passed");
        crate::player::advance_engine_voice(app, book_id);
    }
}
