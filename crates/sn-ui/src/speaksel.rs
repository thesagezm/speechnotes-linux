//! Speak from selection.
//!
//!  this whole module is authored — the
//! user's round-4 directive ("highlight/select a word or paragraph, the same
//! gesture as copy/cut, and speech starts from it") never had an original
//! implementation to recover; the closest surviving surface is the
//! right-click "Speak from here" menu in `player.rs`, which this module
//! builds on and reuses.
//!
//! The rule everywhere is the same: **the selection start becomes the
//! session's start offset.** The session already takes one (`play_note_from`
//! for notes, `start_session_for_text` for books) — nothing here touches the
//! TTS pipeline. What each surface needs is a different way of *finding the
//! offset*:
//!
//! * a text view's buffer selection is already an offset into exactly the
//!   text a session would speak (the editor's buffer *is* the note's text;
//!   a read-along view's buffer *is* the chapter's text) — the same mapping
//!   the right-click menu uses;
//! * the rendered note preview's labels are selectable but their text is the
//!   *parsed* markdown, so the selected text is searched for in the note's
//!   own text (whitespace-normalised on both sides, longest first);
//! * the EPUB's book page is a webview: the injected viewer script reports
//!   the selection's text, which is searched for in the parsed chapter text
//!   the same way;
//! * a deck's DOM carries the exact `data-s`/`data-e` byte ranges of every
//!   speakable element (the read-along highlight uses them), so a selection
//!   there maps to an exact byte of the slide's speakable text — no search;
//! * a PDF's rendered pages are rasterised PNGs with no selectable text at
//!   all, so there the click on a page *is* the selection: speak from that
//!   page's first byte once its text has been extracted.
//!
//! The affordances: a small "Speak from here" pill pops beside a finished
//! mouse selection (text views and, inside the webviews, the page itself),
//! the right-click menus stay as they were, and F5 with an active selection
//! starts from it (`speak_from_selection`, which returns `false` when
//! nothing is selected so the plain F5 behavior is untouched).

use std::cell::RefCell;
use std::rc::Rc;

use gtk4::glib;
use gtk4::prelude::*;

use crate::app::App;

/// How long a selection must stay still before the pill pops. The buffer's
/// selection mark moves on every drag step; the pill must not chase it.
const SELECTION_QUIET_MS: u64 = 300;

/// How the selection-change reports from the webviews reach the keyboard
/// path. F5 cannot ask a web page what is selected, so the injected scripts
/// post every selection as it settles and the last one is kept here.
#[derive(Clone, Debug)]
pub enum WebSelection {
    /// The EPUB's book page: the selection's text, to be found in the
    /// chapter's parsed text.
    Book { text: String },
    /// A deck: the slide and the exact byte its own highlight ranges gave.
    Deck { slide: usize, byte: usize },
}

// UI-thread state: the webviews report from the main loop, F5 reads on it.
thread_local! {
    static PENDING: RefCell<Option<WebSelection>> = const { RefCell::new(None) };
}

// ---------------------------------------------------------------------------
// Locating a selection inside the text a session will speak
// ---------------------------------------------------------------------------

/// Find where `snippet` (a selection's text, from a rendered surface that
/// rewrites or reflows the source) begins in `text` (the text a session
/// would speak). Returns the byte offset of the match's first byte.
///
/// Both sides are whitespace-normalised first — the preview's labels and the
/// EPUB's parsed chapter reflow the source — and the search tries the whole
/// selection, then progressively shorter prefixes: a long selection can
/// straddle markdown syntax that normalisation cannot remove, but its first
/// words are still findable.
pub fn byte_for_snippet(text: &str, snippet: &str) -> Option<usize> {
    let needle = snippet.trim();
    if needle.is_empty() {
        return None;
    }
    let (norm_text, map) = sn_format::sanitizer::normalize_whitespace(text);
    let (norm_needle, _) = sn_format::sanitizer::normalize_whitespace(needle);
    let mut candidates: Vec<String> = Vec::new();
    let push = |candidates: &mut Vec<String>, take: usize| {
        let candidate: String = norm_needle.chars().take(take).collect();
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    };
    push(&mut candidates, usize::MAX);
    push(&mut candidates, 120);
    push(&mut candidates, 60);
    push(&mut candidates, 24);
    // A final fallback: the selection's first word alone. Short needles hit
    // often, so one at least four characters — anything shorter finds the
    // wrong place more often than the right one.
    if let Some(word) = norm_needle
        .split_whitespace()
        .next()
        .filter(|w| w.chars().count() >= 4)
    {
        if !candidates.iter().any(|c| c == word) {
            candidates.push(word.to_string());
        }
    }
    for candidate in &candidates {
        if let Some(pos) = norm_text.find(candidate.as_str()) {
            return Some(sn_format::sanitizer::original_offset(
                &norm_text,
                &map,
                pos,
            ));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Starting a session from a selection
// ---------------------------------------------------------------------------

/// F5 with a selection: start from it on whichever surface is visible, and
/// report whether a selection was found. `false` means "nothing selected" —
/// the caller falls through to the plain play path.
pub fn speak_from_selection(app: &Rc<App>) -> bool {
    // 1. A text view on the visible surface (the editor, or a reader's
    //    read-along text page): its selection is already in the right text.
    if let Some((view, is_book)) = visible_text_view(app) {
        if view.buffer().has_selection() {
            crate::player::speak_from_cursor(app, &view, is_book);
            return true;
        }
    }
    // 2. The rendered preview: a selectable label under the focus.
    if preview_visible(app) {
        if let Some(snippet) = label_selected_text(focused_label(app).as_ref()) {
            speak_note_from_snippet(app, &snippet);
            return true;
        }
    }
    // 3. A webview's last reported selection (the EPUB's book, a deck).
    let web = PENDING.with(|p| p.borrow_mut().take());
    if let Some(sel) = web {
        match sel {
            WebSelection::Book { text } => {
                speak_book_from_snippet(app, &text);
                return true;
            }
            WebSelection::Deck { slide, byte } => {
                speak_deck_from(app, slide, byte);
                return true;
            }
        }
    }
    false
}

/// The selected note passage found in the note's own text: speech starts
/// there. `false` when the passage could not be located (the caller shows
/// why).
pub fn speak_note_from_snippet(app: &Rc<App>, snippet: &str) -> bool {
    let Some(note) = app.open_note() else {
        return false;
    };
    let text = note.text.clone();
    let Some(byte) = byte_for_snippet(&text, snippet) else {
        note_status(
            app,
            "Could not find that passage in the note — select it in the editor instead.",
        );
        return true;
    };
    let char_offset = text[..byte].chars().count();
    crate::player::play_note_from(app, char_offset);
    true
}

/// The selected book passage found in the open chapter's text: the chapter's
/// session starts there, read-along mode or not.
pub fn speak_book_from_snippet(app: &Rc<App>, snippet: &str) -> bool {
    let (book_id, chapter_text) = {
        let state = app.reader.borrow();
        (
            state.book_id.clone().unwrap_or_default(),
            state.text.clone(),
        )
    };
    if chapter_text.trim().is_empty() {
        crate::reader::set_status(app, "Open a book to speak from it.");
        return true;
    }
    let Some(byte) = byte_for_snippet(&chapter_text, snippet) else {
        crate::reader::set_status(
            app,
            "That passage is not on this page — press play to speak it.",
        );
        return true;
    };
    if !sn_format::sanitizer::is_speakable(&chapter_text[byte..]) {
        crate::reader::set_status(app, "Nothing left to speak from there.");
        return true;
    }
    if let Err(e) =
        crate::player::start_session_for_text(app, book_id, chapter_text, byte)
    {
        crate::reader::set_status(app, &e);
    }
    true
}

/// A deck selection at an exact byte of slide `slide`'s speakable text. A
/// selection may sit on any slide of the document, so the deck steps there
/// first — the same path the slide keys take, which refreshes the slide
/// text, the status and the presenter too.
pub fn speak_deck_from(app: &Rc<App>, slide: usize, byte: usize) -> bool {
    let current = app.reader.borrow().chapter;
    if slide != current {
        crate::reader::go_to_chapter(app, slide as i64 - current as i64);
    }
    let (book_id, text) = {
        let state = app.reader.borrow();
        (
            state.book_id.clone().unwrap_or_default(),
            state.text.clone(),
        )
    };
    if text.trim().is_empty() || byte >= text.len() {
        crate::reader::set_status(app, "Nothing speakable in that selection.");
        return true;
    }
    if !sn_format::sanitizer::is_speakable(&text[byte..]) {
        crate::reader::set_status(app, "Nothing left to speak from there.");
        return true;
    }
    if let Err(e) = crate::player::start_session_for_text(app, book_id, text, byte) {
        crate::reader::set_status(app, &e);
    }
    true
}

/// A PDF page's extracted text landed for a "speak from this page" request:
/// the session starts at the page's first byte. A page with no text (a scan
/// the recogniser has not read yet) looks ahead, exactly as the chapter
/// path does.
pub fn on_speak_page_text(app: &Rc<App>, gen: u64, page: usize, text: String) {
    let (book_id, on_page) = {
        let state = app.reader.borrow();
        (
            state.book_id.clone().unwrap_or_default(),
            state.generation == gen && state.chapter == page,
        )
    };
    if !on_page {
        return;
    }
    if text.trim().is_empty() || !sn_format::sanitizer::is_speakable(&text) {
        crate::reader::set_status(
            app,
            "This page has no text — looking ahead for the first one that does…",
        );
        crate::player::speak_from_first_speakable(book_id, page);
        return;
    }
    app.reader.borrow_mut().text = text.clone();
    if let Some(w) = app.widgets.borrow().clone() {
        w.reader.pdf_text.buffer().set_text(&text);
    }
    if let Err(e) = crate::player::start_session_for_text(app, book_id, text, 0) {
        crate::reader::set_status(app, &e);
    }
}

// ---------------------------------------------------------------------------
// The webviews' selection reports
// ---------------------------------------------------------------------------

/// The EPUB's book page reported a selection. `speak_now` is the bubble's
/// click; otherwise this only arms F5.
pub fn on_epub_selection(app: &Rc<App>, speak_now: bool, text: &str) {
    if text.trim().is_empty() {
        PENDING.with(|p| *p.borrow_mut() = None);
        return;
    }
    PENDING.with(|p| {
        *p.borrow_mut() = Some(WebSelection::Book {
            text: text.to_string(),
        })
    });
    if speak_now {
        speak_book_from_snippet(app, text);
    }
}

/// A deck reported a selection: the slide and the exact byte (or `None`
/// when the selection is not over speakable content).
pub fn on_deck_selection(
    app: &Rc<App>,
    speak_now: bool,
    slide: Option<(usize, usize)>,
) {
    match slide {
        Some((slide, byte)) => {
            PENDING.with(|p| *p.borrow_mut() = Some(WebSelection::Deck { slide, byte }));
            if speak_now {
                speak_deck_from(app, slide, byte);
            }
        }
        None => {
            PENDING.with(|p| *p.borrow_mut() = None);
        }
    }
}

// ---------------------------------------------------------------------------
// The affordances
// ---------------------------------------------------------------------------

/// The "Speak from here" pill beside a finished selection: the buffer's
/// selection mark moving arms a short quiet timer, and a selection that
/// survives it pops the pill at its start. Clicking it is the right-click
/// menu's work — the same `speak_from_cursor`.
pub fn attach_selection_affordance(app: &Rc<App>, view: &gtk4::TextView, is_book: bool) {
    let popover = gtk4::Popover::new();
    let item = gtk4::Button::with_label("Speak from here");
    item.add_css_class("flat");
    popover.set_child(Some(&item));
    popover.set_parent(view);

    let quiet: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    {
        let popover = popover.clone();
        let view = view.clone();
        let quiet = quiet.clone();
        view.buffer().connect_mark_set(move |buffer, _iter, mark| {
            // The selection *is* this mark moving; the caret moves it too,
            // and the has-selection check below sorts the two out.
            if mark.name().as_deref() != Some("selection_bound") {
                return;
            }
            if let Some(old) = quiet.borrow_mut().take() {
                old.remove();
            }
            let popover = popover.clone();
            let view = view.clone();
            let buffer = buffer.clone();
            let quiet_source = quiet.clone();
            let source = glib::timeout_add_local(
                std::time::Duration::from_millis(SELECTION_QUIET_MS),
                move || {
                    *quiet_source.borrow_mut() = None;
                    if buffer.has_selection() {
                        popup_at_selection(&popover, &view);
                    } else {
                        popover.popdown();
                    }
                    glib::ControlFlow::Break
                },
            );
            *quiet.borrow_mut() = Some(source);
        });
    }
    {
        let app = app.clone();
        let popover = popover.clone();
        let view = view.clone();
        item.connect_clicked(move |_| {
            popover.popdown();
            crate::player::speak_from_cursor(&app, &view, is_book);
        });
    }
}

/// The right-click "Speak from this page" on a PDF's rendered pages: a page
/// is an image with no selectable text, so the click *is* the selection, and
/// the page's own first byte is where speech starts.
pub fn attach_pdf_page_speak(
    app: &Rc<App>,
    scroll: &gtk4::ScrolledWindow,
    pdf: &crate::pdfview::PdfView,
) {
    let click = gtk4::GestureClick::new();
    click.set_button(3);
    {
        let app = app.clone();
        let pdf = pdf.clone();
        let scroll = scroll.clone();
        click.connect_pressed(move |_, _n, _x, y| {
            // The gesture's y is the scroll viewport's; the document's is the
            // scroll's own value added to it.
            let doc_y = scroll.vadjustment().value() + y as f64;
            let page = pdf.page_at_doc_y(doc_y);
            crate::reader::speak_from_page(&app, page);
        });
    }
    scroll.add_controller(click);
}

/// The rendered preview's right-click menu: the label under the pointer that
/// holds a selection is the passage, and the note's session starts at it.
pub fn attach_preview_speak(app: &Rc<App>, preview_box: &gtk4::Box) {
    let popover = gtk4::Popover::new();
    let item = gtk4::Button::with_label("Speak from here");
    item.add_css_class("flat");
    popover.set_child(Some(&item));
    popover.set_parent(preview_box);
    {
        let popover = popover.clone();
        let box_for_click = preview_box.clone();
        let box_walk = box_for_click.clone();
        let click = gtk4::GestureClick::new();
        click.set_button(3);
        click.connect_pressed(move |_, _, x, y| {
            let target =
                label_with_selection_at(box_walk.upcast_ref(), x as f64, y as f64);
            if target.is_some() {
                popover.set_pointing_to(Some(&gtk4::gdk::Rectangle::new(
                    x as i32,
                    y as i32,
                    1,
                    1,
                )));
                popover.popup();
            }
        });
        box_for_click.add_controller(click);
    }
    {
        let app = app.clone();
        let popover = popover.clone();
        let box_for_menu = preview_box.clone();
        item.connect_clicked(move |_| {
            popover.popdown();
            // The selected label keeps the focus while the popover is up, so
            // the focus lookup finds it; the walk is the fallback for a
            // selection on a label the pointer never focused.
            let snippet = label_selected_text(focused_label(&app).as_ref()).or_else(|| {
                first_selected_label(box_for_menu.upcast_ref())
                    .and_then(|l| label_selected_text(Some(&l)))
            });
            if let Some(snippet) = snippet {
                speak_note_from_snippet(&app, &snippet);
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Text-view / label plumbing
// ---------------------------------------------------------------------------

/// The text view the user is looking at, if it can hold a selection: the
/// editor, or a reader's read-along text page. The bool is the book flag the
/// speak path needs.
fn visible_text_view(app: &Rc<App>) -> Option<(gtk4::TextView, bool)> {
    let w = app.widgets.borrow().clone()?;
    match w.editor_stack.visible_child_name()?.as_str() {
        "editor" => Some((w.editor_text.clone(), false)),
        "reader" => {
            let view = match w.reader.stack.visible_child_name()?.as_str() {
                "text" => Some(w.reader.epub_text.clone()),
                "pdf-text" => Some(w.reader.pdf_text.clone()),
                "slides-text" => Some(w.reader.slides_text.clone()),
                _ => None,
            };
            view.map(|v| (v, true))
        }
        _ => None,
    }
}

/// Whether the rendered preview is the visible page.
fn preview_visible(app: &Rc<App>) -> bool {
    app.widgets
        .borrow()
        .as_ref()
        .and_then(|w| w.editor_stack.visible_child_name())
        .as_deref()
        == Some("preview")
}

/// The window's focus widget, when it is a label.
fn focused_label(app: &Rc<App>) -> Option<gtk4::Label> {
    let w = app.widgets.borrow().clone()?;
    let root = w.editor_stack.root()?.downcast::<gtk4::Window>().ok()?;
    gtk4::prelude::GtkWindowExt::focus(&root)?
        .downcast::<gtk4::Label>()
        .ok()
}

/// The selected text of `label`, if it is selectable and something is
/// selected. The selection bounds are indices into the label's text; they
/// are widened to character boundaries before slicing.
fn label_selected_text(label: Option<&gtk4::Label>) -> Option<String> {
    let label = label?;
    let (start, end) = label.selection_bounds()?;
    let text = label.text();
    let widen = |mut i: usize, forward: bool| -> usize {
        if forward {
            while i < text.len() && !text.is_char_boundary(i) {
                i += 1;
            }
        } else {
            while i > 0 && !text.is_char_boundary(i) {
                i -= 1;
            }
        }
        i.min(text.len())
    };
    let (start, end) = (start.max(0) as usize, end.max(0) as usize);
    let (start, end) = if start <= end {
        (widen(start, false), widen(end, true))
    } else {
        (widen(end, false), widen(start, true))
    };
    if start >= end {
        return None;
    }
    let selected = &text[start..end];
    // A list item's marker rides inside its label (that is what makes a big
    // note open in a second — see `markup_label_with_marker`), so a
    // selection that starts at the very beginning would carry the bullet,
    // the tick box or the item number into the spoken snippet. The voice
    // should read the item, not its decoration.
    let selected = strip_list_marker(selected);
    if selected.is_empty() {
        return None;
    }
    Some(selected.to_string())
}

/// Drop a leading list marker from a snippet taken out of the preview.
///
/// The markers are the ones the preview draws: the bullet, the two task
/// boxes, and an ordered item's number.
fn strip_list_marker(text: &str) -> &str {
    for marker in ["• ", "☐ ", "☑ "] {
        if let Some(rest) = text.strip_prefix(marker) {
            return rest;
        }
    }
    let digits = text.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 {
        let rest = &text[digits..];
        for marker in [". ", ") ", ": "] {
            if let Some(tail) = rest.strip_prefix(marker) {
                return tail;
            }
        }
    }
    text
}

/// The selectable label holding a selection at `(x, y)` in `root`'s
/// coordinates, if there is one.
fn label_with_selection_at(root: &gtk4::Widget, x: f64, y: f64) -> Option<gtk4::Label> {
    walk_labels(root, &mut |label, bounds| {
        bounds.contains_point(&gtk4::graphene::Point::new(x as f32, y as f32))
                && label.selection_bounds().is_some()
    })
}

/// Any descendant of `root` that currently holds a selection.
fn first_selected_label(root: &gtk4::Widget) -> Option<gtk4::Label> {
    walk_labels(root, &mut |label, _bounds| label.selection_bounds().is_some())
}

/// Depth-first walk over `root`'s label descendants; `pick` decides. The
/// bounds handed to it are in `root`'s coordinates.
fn walk_labels(
    root: &gtk4::Widget,
    pick: &mut dyn FnMut(&gtk4::Label, &gtk4::graphene::Rect) -> bool,
) -> Option<gtk4::Label> {
    let children_list = root.observe_children();
    let mut children = Vec::new();
    for i in 0..children_list.n_items() {
        if let Some(obj) = children_list.item(i) {
            if let Ok(widget) = obj.downcast::<gtk4::Widget>() {
                children.push(widget);
            }
        }
    }
    for child in children {
        let Some(bounds) = child.compute_bounds(root) else {
            continue;
        };
        if let Ok(label) = child.clone().downcast::<gtk4::Label>() {
            if pick(&label, &bounds) {
                return Some(label);
            }
        }
        if let Some(found) = walk_labels(&child, pick) {
            return Some(found);
        }
    }
    None
}

/// Position the pill at the selection's first character and pop it.
fn popup_at_selection(popover: &gtk4::Popover, view: &gtk4::TextView) {
    let buffer = view.buffer();
    let Some((start, _end)) = buffer.selection_bounds() else {
        return;
    };
    let location = view.iter_location(&start);
    let (x, y) = view.buffer_to_window_coords(
        gtk4::TextWindowType::Widget,
        location.x(),
        location.y(),
    );
    popover.set_pointing_to(Some(&gtk4::gdk::Rectangle::new(x, y - 8, 1, 1)));
    popover.popup();
}

/// The note transport's status line (the same line the player writes).
fn note_status(app: &Rc<App>, text: &str) {
    if let Some(widgets) = app.widgets.borrow().clone() {
        widgets.player.status.set_text(text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snippet_is_found_through_reflow_and_markup() {
        // The preview's labels reflow the markdown: bold markers are gone
        // and whitespace is collapsed. The passage must still be found at
        // the offset of its first word in the source.
        let note = "# Title\n\nSome **emphasised** words sit here.\n\nTail text follows.";
        assert_eq!(
            byte_for_snippet(note, "emphasised words sit here."),
            Some(note.find("emphasised").unwrap()),
            "the match begins at the snippet's first word"
        );
    }

    #[test]
    fn a_long_selection_falls_back_to_its_first_words() {
        // A selection straddling markup the normaliser cannot remove: the
        // whole-snippet search misses, the prefix finds the passage.
        let note = "Plain opening line.\n\nA [link](http://x.example) and **bold** together.\n\nEnd.";
        let snippet = "link](http://x.example) and **bold** together.";
        let found = byte_for_snippet(note, snippet);
        assert_eq!(found, Some(note.find("link").unwrap()));
    }

    #[test]
    fn a_short_selection_never_guesses() {
        // A needle that is nowhere in the note is not "found" somewhere
        // near: no prefix and no first-word fallback may invent a match.
        // ("d s" would be a bad probe — it occurs inside "Second sentence".)
        let note = "First sentence here. Second sentence follows.";
        assert_eq!(byte_for_snippet(note, "qq zz"), None);
    }

    #[test]
    fn empty_and_absent_snippets_are_none() {
        let note = "Some text worth speaking.";
        assert_eq!(byte_for_snippet(note, ""), None);
        assert_eq!(byte_for_snippet(note, "   \n  "), None);
        assert_eq!(byte_for_snippet(note, "not present anywhere"), None);
    }

    #[test]
    fn whitespace_differences_do_not_defeat_the_match() {
        let note = "Line one.\n  Line   two  continues.\nLine three.";
        let offset = byte_for_snippet(note, "Line two continues.");
        let cleaned = note[offset.unwrap()..].to_string();
        assert!(cleaned.starts_with("Line   two"));
    }

    #[test]
    fn a_selection_that_starts_on_a_list_marker_does_not_speak_it() {
        // A list row's marker rides inside its label (one label per row is
        // what makes a big note open in a second), so a selection that spans
        // the label from the very start would otherwise carry the decoration
        // into the spoken snippet. The voice reads the item, not its marker.
        assert_eq!(strip_list_marker("• buy milk"), "buy milk");
        assert_eq!(strip_list_marker("☐ unfiled thing"), "unfiled thing");
        assert_eq!(strip_list_marker("☑ done thing"), "done thing");
        assert_eq!(strip_list_marker("1. first item"), "first item");
        assert_eq!(strip_list_marker("12. twelfth item"), "twelfth item");
        assert_eq!(strip_list_marker("2) parenthesised"), "parenthesised");

        // A selection that does not begin on the marker — the common case,
        // and every non-list paragraph — must come through untouched. A
        // digit that opens ordinary prose is not an item number.
        assert_eq!(strip_list_marker("2026 was the year"), "2026 was the year");
        assert_eq!(strip_list_marker("buy milk"), "buy milk");
    }
}
