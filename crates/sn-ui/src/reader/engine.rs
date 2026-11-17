//! The engine-rendered documents: the Mobipocket/Kindle family, FB2, RTF,
//! HTML/HTMLZ and plain text, shown through the book view — foliate-js in
//! the reader's web engine.
//!
//! The old path *parsed* each of these formats into a plain-XHTML model and
//! served synthesised pages, and the parse was where the bugs lived: a
//! PalmDOC decompressor that emitted repeated letters, an XML reduction
//! that leaked `xmlns` into prose, a chapter heuristic that listed a book's
//! front matter as chapters, images that never arrived. The view now hands
//! the file's own bytes to the engine and shows what the engine makes of
//! them — readest's approach, vendored (see `crates/sn-ui/assets/foliate-js`).
//!
//! Two surfaces, one book — the same split the EPUB view runs on: the
//! **webview is the book** (what you look at), and the engine's own text
//! feeds the voice and the read-along view. The text arrives from the
//! engine itself (`text:` reports), so what the listener hears is exactly
//! what the page shows, decompression and all.
//!
//! The book report (`book:`) is the format's own nested contents — a KF8's
//! NCX, an FB2's sections, a TXT's detected headings — and it is written
//! into the manifest's TOC, so the sidebar shows chapters within chapters
//! from the book's real structure, not the old heuristic's guess.

use std::rc::Rc;

use gtk4::prelude::*;

use crate::app::App;
use crate::bookview::{BookInfo, Relocate};
use super::*;

/// Open a document in the engine, resuming at `index` (a best-effort
/// section hint; the CFI resume, when the manifest carries one, wins).
///
/// A book the engine already has open is not reloaded — a re-open would
/// re-run the engine's parse (a PalmDOC decompresses everything at once) —
/// it is navigated to, exactly what the auto-advance and a forward walk
/// want.
pub(crate) fn open_document(app: &Rc<App>, book: &sn_store::books::Book, index: usize, text: &str) {
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    let reader = &w.reader;
    reader.pdf_tools.set_visible(false);
    // The page-progress bar belongs to a PDF; the engine reports its own
    // position through relocations.
    reader.page_bar.set_visible(false);
    reader.epub_tools.set_visible(true);
    reader.present.set_visible(false);
    w.reader.slides.end_show();
    sync_show_controls(app);
    reader.title.set_text(&book.manifest.title);

    {
        let mut state = app.reader.borrow_mut();
        state.engine_rendered = true;
        state.engine_ready = false;
    }
    // The engine already holds this book: navigate, never reload. The
    // relocation that follows reports the new section and everything above
    // follows it; the read-along text lands with the section's text report.
    let already = {
        let state = app.reader.borrow();
        state.engine_ready && state.book_id.as_deref() == Some(book.manifest.id.as_str())
    };
    if already {
        reader.book.go_to_index(index);
        reader.stack.set_visible_child_name("engine");
        return;
    }

    // The resume CFI is the engine's own position — exact through reflow —
    // with the chapter index as the fallback the engine resolves when the
    // CFI cannot be.
    let cfi = book.manifest.position.cfi.clone();
    reader.book.set_appearance(appearance_from_prefs(&app.prefs.get()));
    let path = book.original();
    reader.book.open(
        &path,
        &book.manifest.format,
        &book.manifest.title,
        &book.manifest.author,
        cfi.as_deref(),
    );
    // The parsed chapter starts the read-along view (and seeds the voice if
    // this open was asked to speak before the engine's text lands).
    reader.epub_text.buffer().set_text(text);
    reader.stack.set_visible_child_name("engine");
    reader
        .status
        .set_text(&format!("Opening…"));
    //  exchanged with the TOC button's icon (user report).
    reader.mode_toggle.set_icon_name("view-contents-symbolic");
    reader
        .mode_toggle
        .set_tooltip_text(Some("Read along with the engine's text"));
}

/// The engine's book report: metadata plus the format's own nested
/// contents. The report's rows replace the manifest's TOC — the real
/// chapters, the real depths — and the sidebar rebuilds from them.
fn on_book_report(app: &Rc<App>, info: BookInfo) {
    let Ok(Some(book)) = current_book(app) else {
        return;
    };
    if !app.reader.borrow().engine_rendered {
        return;
    }
    let BookInfo {
        section_count,
        toc,
        ..
    } = info;
    if section_count == 0 {
        return;
    }
    // The manifest learns the engine's shape: its section count is what the
    // controls clamp against, and the TOC rows are the book's own — label,
    // section index, depth, and the href the engine navigates by.
    let mut manifest = book.manifest.clone();
    manifest.spine_count = section_count;
    manifest.spine = (0..section_count).map(|i| format!("section-{i}")).collect();
    manifest.toc = toc
        .iter()
        .map(|row| sn_store::books::ManifestTocEntry {
            spine_index: Some(row.index),
            href: row.href.clone(),
            label: row.label.clone(),
            depth: row.depth,
            fragment: None,
        })
        .collect();
    {
        let mut state = app.reader.borrow_mut();
        state.engine_ready = true;
    }
    // The sidebar reads the manifest's TOC; rebuild it with the real rows.
    let updated = sn_store::books::BooksStore::from_manifest(manifest);
    build_toc(app, &updated);
    refresh_controls(app);
    // Persist: the engine TOC survives restarts, so the shelf's next open
    // starts with the real contents. Off the UI thread, like every
    // manifest write.
    let manifest = updated.manifest.clone();
    crate::tasks::spawn_detached(move || {
        let book = sn_store::books::BooksStore::from_manifest(manifest);
        let _ = sn_store::books::BooksStore::save(&book);
    });
}

/// A relocation: the engine moved — a page turn, a scroll, a navigation, a
/// sidebar row. The reader's chapter, the status line, the marked sidebar
/// row and the saved position all follow it.
pub(crate) fn on_engine_relocate(app: &Rc<App>, rel: Relocate) {
    if !app.reader.borrow().engine_rendered {
        return;
    }
    // A page turn asked for while the voice reads along: this relocation is
    // the page the move landed on, and it is where the voice goes. Consumed
    // whatever the voice's state — a flag set without a relocation to answer
    // it must not survive to hijack a later one.
    let follow = {
        let mut state = app.reader.borrow_mut();
        std::mem::replace(&mut state.voice_follows_page, false)
    };
    let changed = {
        let mut state = app.reader.borrow_mut();
        let changed = state.chapter != rel.index;
        state.chapter = rel.index;
        changed
    };
    if changed {
        // The voice follows into the new section when the turn was its
        // steering: the section's own text report starts it there.
        if follow && voice_reading_along(app) {
            let gen = app.reader.borrow().generation;
            // The reader steered the voice by hand: the walk's own position
            // ends here — the report speaks the section the reader chose.
            let mut state = app.reader.borrow_mut();
            state.walk_section = None;
            state.speak_when_opened = Some(gen);
        }
        on_engine_chapter_changed(app, rel.index);
        return;
    }
    let status = engine_status(&rel);
    if let Some(w) = app.widgets.borrow().clone() {
        w.reader.status.set_text(&status);
        mark_toc_row(app, rel.index);
        refresh_controls(app);
    }
    schedule_save_book_position(app, &rel);
    if follow && voice_reading_along(app) {
        follow_voice_to_page(app);
    }
}

/// Whether the voice is on this book — playing or paused. The steppers'
/// read-along steering and the relocation follow both mean this: a voice
/// the reader has engaged, not merely a book that once spoke.
fn voice_reading_along(app: &Rc<App>) -> bool {
    app.tts.borrow().is_some() && app.tts_book.get()
}

/// The steppers moved the page while the voice was reading along, and the
/// turn landed inside the section it was reading: the voice restarts on the
/// page the reader landed on — its first text. (A turn that crossed into a
/// new section never reaches here; that path hands the voice to the new
/// section's text report.)
fn follow_voice_to_page(app: &Rc<App>) {
    // A user's stepper turn re-targets the voice by hand: any walk the last
    // session left under way ends here — the next walk resumes from the
    // section the reader steered into.
    app.reader.borrow_mut().walk_section = None;
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    let callback_app = app.clone();
    w.reader.book.page_offset(move |index, offset| {
        let Ok(Some(book)) = current_book(&callback_app) else {
            return;
        };
        let (chapter, text) = {
            let state = callback_app.reader.borrow();
            (state.chapter, state.text.clone())
        };
        // The relocation and the view's own position must name the same
        // section; a race between them leaves the state's text the only
        // pair that is consistent, and it is not worth a voice starting on
        // half-moved state.
        if index != chapter {
            return;
        }
        let resume = offset
            .map(|units| sn_store::model::utf16_to_byte_offset(&text, units as i64))
            .unwrap_or(0);
        let remainder = text.get(resume..).unwrap_or_default();
        if !sn_format::sanitizer::is_speakable(remainder) {
            // A page with nothing on it stops the voice rather than leaving
            // it reading a page the reader has already left.
            crate::player::stop_playback(&callback_app);
            set_status(&callback_app, "This page has no text — the voice stops here.");
            return;
        }
        if let Err(e) = crate::player::start_session_for_text(
            &callback_app,
            book.manifest.id.clone(),
            text,
            resume,
        ) {
            set_status(&callback_app, &e);
        }
    });
}

/// The engine moved to a section it was not on: the read-along text and the
/// voice's text follow (from the engine's cache when the section is already
/// rendered, else from the section's text report when it loads).
pub(crate) fn on_engine_chapter_changed(app: &Rc<App>, index: usize) {
    let callback_app = app.clone();
    if let Some(w) = app.widgets.borrow().clone() {
        w.reader.book.section_text(index, move |index, loaded, text| {
            on_section_text_fetched(&callback_app, index, loaded, text);
        });
        mark_toc_row(app, index);
        refresh_controls(app);
    }
}

/// A section's text, from the engine. The voice and the read-along view
/// read exactly what the page shows.
pub(crate) fn on_section_text(app: &Rc<App>, index: usize, text: String) {
    on_section_text_inner(app, index, true, text);
}

/// The cache-read path (`sectionText`, asked on a section change): the
/// shell's `loaded` flag is the truth — an unloaded section is still
/// rendering and its report decides later, so an unloaded answer never
/// clobbers the state's text nor starts the voice.
pub(crate) fn on_section_text_fetched(
    app: &Rc<App>,
    index: usize,
    loaded: bool,
    text: String,
) {
    on_section_text_inner(app, index, loaded, text);
}

fn on_section_text_inner(
    app: &Rc<App>,
    index: usize,
    authoritative: bool,
    text: String,
) {
    let empty = text.trim().is_empty();
    {
        let mut state = app.reader.borrow_mut();
        if !state.engine_rendered
            // The chapter gate: the report must be for the section the
            // reader (or the voice) is on. The voice's walk is its own
            // position — the relocation answering the walk's steering names
            // the section the visible PAGE starts in, which a
            // continuously-paginated book lays elsewhere, so the walk's own
            // section accepts its report too. Without this the walk's
            // still-loading fetch waited for a report the gate dropped, and
            // the book went silent mid-front-matter (measured 2026-11-17).
            || (state.chapter != index && state.walk_section != Some(index))
        {
            return;
        }
        // The shell's report is the truth about the section — an empty one
        // clears the read-along. A cache-read's emptiness is only "still
        // loading", and the state keeps the text it already holds.
        if !empty || authoritative {
            state.text = text.clone();
        }
    }
    if let Some(w) = app.widgets.borrow().clone() {
        if !empty || authoritative {
            w.reader.epub_text.buffer().set_text(&text);
        }
    }
    // A "speak when opened" ask waits for the section's own words: an empty
    // fetch is a section still loading, not a silent one — the report says
    // whether the voice speaks or the walk passes the section by.
    if empty && !authoritative {
        return;
    }
    let speak_now = {
        let mut state = app.reader.borrow_mut();
        state.speak_when_opened.take().is_some()
    };
    if speak_now {
        if !empty && sn_format::sanitizer::is_speakable(&text) {
            {
                let mut state = app.reader.borrow_mut();
                state.voice_walk_skips = 0;
            }
            // The section top, not the page hop: this ask belongs to the
            // voice's walk (or to an open the voice asked for), and the
            // section it names may share the page the reader is still
            // looking at — `speak_chapter` would resume from the visible
            // page's offset, and `speak_chapter` is also where a
            // user-started voice ends the walk. `speak_chapter_from` keeps
            // the walk's own position untouched.
            crate::reader::speak_chapter_from(app, Some(0));
        } else {
            // A section the engine has no words for — a cover, a blank page
            // — is passed, not fatal: the walk moves to the next section
            // and the engine's own text keeps deciding.
            let book_id = current_book(app)
                .ok()
                .flatten()
                .map(|book| book.manifest.id)
                .unwrap_or_default();
            if !book_id.is_empty() {
                crate::player::advance_engine_voice(app, &book_id);
            }
        }
    }
}

/// The status line from a relocation: the book's own TOC label when the
/// position carries one, the chapter line either way.
fn engine_status(rel: &Relocate) -> String {
    let count = rel.section_count.max(1);
    let position = format!("{} / {}", rel.index + 1, count);
    if rel.label.trim().is_empty() {
        format!("chapter {position}")
    } else {
        format!("{} · {}", rel.label.trim(), position)
    }
}

/// The trailing manifest write for a relocation, debounced 1.2 s — the same
/// cadence the PDF's page turns save on.
pub(crate) fn schedule_save_book_position(app: &Rc<App>, rel: &Relocate) {
    let rel = rel.clone();
    if let Some(old) = app.book_save_source.borrow_mut().take() {
        old.remove();
    }
    let app2 = app.clone();
    let source = glib::timeout_add_local(
        std::time::Duration::from_millis(1200),
        move || {
            *app2.book_save_source.borrow_mut() = None;
            save_book_position(&app2, &rel);
            glib::ControlFlow::Break
        },
    );
    *app.book_save_source.borrow_mut() = Some(source);
}

/// Write the engine's position now, off the UI thread: the CFI is the
/// resume anchor, the section index the fallback and the shelf's count.
pub(crate) fn save_book_position(app: &Rc<App>, rel: &Relocate) {
    let Ok(Some(book)) = current_book(app) else {
        return;
    };
    let mut manifest = book.manifest.clone();
    manifest.position.chapter_index = rel.index;
    manifest.position.chapter_fraction = rel.fraction.clamp(0.0, 1.0);
    if !rel.cfi.is_empty() {
        manifest.position.cfi = Some(rel.cfi.clone());
    }
    manifest.last_opened_at = sn_core::AppleDate::now().raw();
    crate::tasks::spawn_detached(move || {
        let book = sn_store::books::BooksStore::from_manifest(manifest);
        let _ = sn_store::books::BooksStore::save(&book);
    });
}

/// Wire the engine's callbacks once, at build time. They hold the app, not
/// the widgets (the widgets cell may be empty during teardown).
pub(crate) fn wire_engine_callbacks(app: &Rc<App>, view: &crate::bookview::BookView) {
    {
        let app = app.clone();
        view.on_book_info(move |info| on_book_report(&app, info));
    }
    {
        let app = app.clone();
        view.on_relocate(move |rel| on_engine_relocate(&app, rel));
    }
    {
        let app = app.clone();
        view.on_text(move |index, text| on_section_text(&app, index, text));
    }
    {
        view.on_external(move |href| {
            // The same rule the EPUB view's links follow: desktop schemes
            // only, everything else refused.
            if crate::epubview::is_safe_link(&href) {
                gtk4::UriLauncher::new(&href).launch(
                    gtk4::Window::NONE,
                    gtk4::gio::Cancellable::NONE,
                    |result| {
                        if let Err(e) = result {
                            tracing::warn!(error = %e, "the desktop refused to open a link");
                        }
                    },
                );
            } else {
                tracing::warn!(href = %href, "refusing to follow a link with an unlisted scheme");
            }
        });
    }
    {
        let app = app.clone();
        view.on_selection(move |speak_now, text| {
            crate::speaksel::on_epub_selection(&app, speak_now, &text);
        });
    }
    {
        let app = app.clone();
        view.on_error(move |message| {
            set_status(&app, &format!("The book could not be opened: {message}"));
        });
    }
}

/// Whether a sidebar row should navigate the engine rather than re-open the
/// book: true exactly when the open book is engine-rendered and the engine
/// holds it — a row with an href then lands where the book's own contents
/// point.
pub(crate) fn engine_row_navigates(app: &Rc<App>) -> bool {
    let state = app.reader.borrow();
    state.engine_rendered && state.engine_ready
}
