//! Right-click → "Speak from here": the voice picks up from the cursor or
//! the selection's first character. Split out of `player.rs` (Round 4,
//! pure code-motion).

use std::rc::Rc;

use gtk4::prelude::*;

use crate::app::App;

use super::session::{play_note_from, start_session_for_text};

pub fn attach_speak_from_here(app: &Rc<App>, view: &gtk4::TextView, is_book: bool) {
    let popover = gtk4::Popover::new();
    let item = gtk4::Button::with_label("Speak from here");
    item.add_css_class("flat");
    popover.set_child(Some(&item));
    popover.set_parent(view);

    {
        let popover = popover.clone();
        let view = view.clone();
        let click = gtk4::GestureClick::new();
        click.set_button(3);
        {
            let popover = popover.clone();
            let view = view.clone();
            click.connect_pressed(move |_, _, x, y| {
                // Widget coords → buffer coords → the iterator under the
                // click, made the cursor so "here" survives until the item
                // is pressed. A live selection is left alone: its first
                // character is the resume point, and re-cursoring would
                // destroy it.
                if !view.buffer().has_selection() {
                    let (bx, by) = view.window_to_buffer_coords(
                        gtk4::TextWindowType::Widget,
                        x as i32,
                        y as i32,
                    );
                    if let Some(iter) = view.iter_at_location(bx, by) {
                        view.buffer().place_cursor(&iter);
                    }
                }
                popover
                    .set_pointing_to(Some(&gtk4::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
                popover.popup();
            });
        }
        view.add_controller(click);
    }
    {
        let app = app.clone();
        let popover = popover.clone();
        let view = view.clone();
        item.connect_clicked(move |_| {
            popover.popdown();
            speak_from_cursor(&app, &view, is_book);
        });
    }
}

/// The menu item's work: the selection start — or the cursor — becomes the
/// resume point, and the session starts there.  pub
/// so speaksel.rs's selection pill (and F5) can run the same path the
/// right-click menu does.
pub fn speak_from_cursor(app: &Rc<App>, view: &gtk4::TextView, is_book: bool) {
    let buffer = view.buffer();
    let char_offset = (if buffer.has_selection() {
        buffer
            .selection_bounds()
            .map(|(start, _)| start.offset())
            .unwrap_or_else(|| buffer.cursor_position())
    } else {
        buffer.cursor_position()
    })
    .max(0) as usize;

    if !is_book {
        play_note_from(app, char_offset);
        return;
    }

    let (book_id, chapter_text) = {
        let state = app.reader.borrow();
        (
            state.book_id.clone().unwrap_or_default(),
            state.text.clone(),
        )
    };
    if chapter_text.trim().is_empty() {
        crate::reader::set_status(app, "Open a book to speak from it.");
        return;
    }
    // The buffer must hold the text the session will speak; the read-along
    // makes the same hash comparison for the same reason — an offset into
    // different text is a voice starting somewhere the listener did not pick.
    let shown = buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), true)
        .to_string();
    if sn_core::util::stable_hash(&shown) != sn_core::util::stable_hash(&chapter_text) {
        crate::reader::set_status(app, "The page text has changed — press play to speak it.");
        return;
    }
    let resume = char_to_byte(&chapter_text, char_offset);
    if !sn_format::sanitizer::is_speakable(&chapter_text[resume..]) {
        crate::reader::set_status(app, "Nothing left to speak from there.");
        return;
    }
    if let Err(e) = start_session_for_text(app, book_id, chapter_text, resume) {
        crate::reader::set_status(app, &e);
    }
}

/// A GTK character offset becomes a byte offset into `text` — the same
/// counting the plan's byte chunks use, the reverse of the read-along's
/// `text[..byte].chars().count()`.
pub(crate) fn char_to_byte(text: &str, char_offset: usize) -> usize {
    text.char_indices()
        .nth(char_offset)
        .map(|(b, _)| b)
        .unwrap_or(text.len())
}
