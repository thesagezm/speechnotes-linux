//! Keyboard shortcuts, the pin/star/delete actions, the sort-button helpers
//! and the slash-menu trigger (split out of `window.rs`, Round 4, pure
//! code-motion).

use std::rc::Rc;

use gtk4::prelude::*;
use gtk4 as gtk;
use gtk4::glib::Propagation;
use gtk4::gdk;

use sn_store::model::{Scope, SortOrder};

use crate::app::{App, View};

use super::editor::{apply_format_action, focus_editor, new_note, set_view, FormatAction};
use super::exporting::{export_open_note, insert_image_into_open_note};
use super::rail::{apply_focus_mode, toggle_column};
use super::sidebar::{refresh_all, refresh_list};

// ---------------------------------------------------------------------------
// Keyboard shortcuts
// ---------------------------------------------------------------------------

pub(crate) fn install_shortcuts(window: &gtk::ApplicationWindow, app: &Rc<App>) {
    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Bubble);
    {
        let app = app.clone();
        controller.connect_key_pressed(move |_, keyval, _keycode, state| {
            let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
            let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
            use gdk::Key;
            // Playback keys need no modifiers: they are transport controls.
            if matches!(keyval, Key::F5 | Key::F6) {
                // PowerPoint show conventions (2026-11-07): Shift+F5
                // starts the show from the current slide — on a deck it
                // toggles the presentation (plain F5 stays the voice).
                if shift {
                    if keyval == Key::F5 && crate::reader::slides_open(&app) {
                        crate::reader::toggle_present(&app);
                        return Propagation::Stop;
                    }
                    return Propagation::Proceed;
                }
                match keyval {
                    Key::F5 => {
                        // The reader owns F5 while it is the visible page: a
                        // chapter's speech is the same session, started from
                        // the chapter rather than from the open note.
                        let reader_visible = app
                            .widgets
                            .borrow()
                            .as_ref()
                            .and_then(|w| w.editor_stack.visible_child_name())
                            .as_deref()
                            == Some("reader");
                        if reader_visible {
                            // An audiobook's transport is the player's: the
                            // same key starts and pauses it, and re-opens a
                            // stopped one from its bookmark.
                            if !crate::abook::maybe_toggle(&app)
                                //  a selection on the
                                // visible surface speaks from itself first;
                                // `false` is "nothing selected" and the
                                // plain chapter play follows.
                                && !crate::speaksel::speak_from_selection(&app)
                            {
                                crate::reader::speak_chapter(&app);
                            }
                        } else if !crate::speaksel::speak_from_selection(&app) {
                            crate::player::toggle_play(&app);
                        }
                    }
                    Key::F6 => {
                        // Whichever session is live: a reader page showing an
                        // audiobook stops the audiobook, everything else the
                        // note player. Stopping only the note player left the
                        // audiobook's ffmpeg child and sink running under an
                        // unrelated book.
                        if crate::abook::is_live(&app) {
                            crate::abook::close(&app);
                        } else {
                            crate::player::stop_playback(&app);
                        }
                    }
                    _ => unreachable!(),
                }
                return Propagation::Stop;
            }
            // Expand-to-window: no modifiers, transport-key style. F11 is
            // what a reader expects to find; the toggle is the mouse route.
            if matches!(keyval, Key::F11) {
                apply_focus_mode(&app, !app.focus_mode.get());
                return Propagation::Stop;
            }
            // Escape leaves fill-the-window on **every** page — a note, a
            // book, a deck — the one key the mode always answers, the way
            // F11 enters it from anywhere. (A deck's webview posts the same
            // exit when IT holds the focus; this arm covers every other
            // focus on every page, which is what used to be deck-only.)
            if !ctrl && !shift && keyval == Key::Escape && app.focus_mode.get() {
                apply_focus_mode(&app, false);
                return Propagation::Stop;
            }
            // The sidebar's own toggle — the rail is its mouse route, F9 the
            // keyboard one (the file-manager convention). A no-op during
            // fill-the-window: there the mode owns both columns, and its own
            // leave routes are F11 and the header's leave button.
            if matches!(keyval, Key::F9) {
                if !app.focus_mode.get() {
                    toggle_column(&app, 0);
                }
                return Propagation::Stop;
            }
            // The reader page's own keys: arrows turn chapters, the vertical
            // keys scroll. This controller is bubble-phase, so it only sees
            // the keys no focused widget consumed — the editor's caret, the
            // search entry and the speed spins keep their arrows — and the
            // guard keeps the rest of the app's keys unmolested.
            if !ctrl && !shift && crate::reader::page_visible(&app) {
                // A deck's keys are the presentation's: every direction
                // steps a slide, the way a projector clicker works. (The
                // webview repeats the same map when it holds focus, so the
                // keys work whichever widget has them.)
                let deck = crate::reader::slides_open(&app);
                match keyval {
                    Key::Left => {
                        crate::reader::go_to_chapter(&app, -1);
                        return Propagation::Stop;
                    }
                    Key::Right => {
                        crate::reader::go_to_chapter(&app, 1);
                        return Propagation::Stop;
                    }
                    Key::Up | Key::Page_Up => {
                        if deck {
                            crate::reader::go_to_chapter(&app, -1);
                            return Propagation::Stop;
                        }
                        if crate::reader::scroll_reader(&app, -1.0) {
                            return Propagation::Stop;
                        }
                        // The page cannot travel (it fits the viewport, or
                        // the view is at its top): the reader is asking to
                        // go back. The same rule Down follows at the foot.
                        crate::reader::go_to_chapter(&app, -1);
                        return Propagation::Stop;
                    }
                    Key::Down | Key::Page_Down | Key::space => {
                        if deck {
                            crate::reader::go_to_chapter(&app, 1);
                            return Propagation::Stop;
                        }
                        if crate::reader::scroll_reader(&app, 1.0) {
                            return Propagation::Stop;
                        }
                        // Space at the foot of a page (or on a page that
                        // does not scroll) turns the page — the paper-book
                        // behaviour the key carries everywhere else. The
                        // old handler ate the key whenever the page had a
                        // viewport at all, so space did nothing exactly
                        // where a reader reaches for it.
                        crate::reader::go_to_chapter(&app, 1);
                        return Propagation::Stop;
                    }
                    _ => {}
                }
            }
            if !ctrl {
                return Propagation::Proceed;
            }
            let handled = match (keyval, shift) {
                (Key::n, false) => {
                    new_note(&app);
                    true
                }
                (Key::N, true) | (Key::n, true) => {
                    crate::dialogs::new_notebook(&app);
                    true
                }
                (Key::f, false) => {
                    if let Some(w) = app.widgets.borrow().clone() {
                        // Searching needs the header back — the search entry
                        // is one of the things focus mode collapsed.
                        if app.focus_mode.get() {
                            apply_focus_mode(&app, false);
                        }
                        w.search.grab_focus();
                    }
                    true
                }
                (Key::P, true) => {
                    toggle_pin(&app);
                    true
                }
                (Key::S, true) => {
                    toggle_star(&app);
                    true
                }
                (Key::D, true) => {
                    delete_open(&app);
                    true
                }
                (Key::_1, false) => {
                    set_view(&app, View::Notes(Scope::All));
                    true
                }
                (Key::_2, false) => {
                    set_view(&app, View::Notes(Scope::Unfiled));
                    true
                }
                (Key::_3, false) => {
                    set_view(&app, View::Bin);
                    true
                }
                (Key::w, false) => {
                    app.flush_all();
                    if let Some(w) = app.widgets.borrow().clone() {
                        w.window.close();
                    }
                    true
                }
                (Key::e, false) => {
                    focus_editor(&app);
                    true
                }
                // Formatting. These are the same `FormatAction`s the bar
                // buttons carry, so the two routes cannot drift apart.
                (Key::b, false) => {
                    apply_format_action(&app, &FormatAction::Wrap("**"));
                    true
                }
                (Key::i, false) => {
                    apply_format_action(&app, &FormatAction::Wrap("*"));
                    true
                }
                (Key::grave, false) => {
                    apply_format_action(&app, &FormatAction::Wrap("`"));
                    true
                }
                (Key::k, false) => {
                    apply_format_action(&app, &FormatAction::Link);
                    true
                }
                (Key::_1, true) => {
                    apply_format_action(&app, &FormatAction::Line("# "));
                    true
                }
                (Key::e, true) => {
                    export_open_note(&app);
                    true
                }
                (Key::i, true) => {
                    insert_image_into_open_note(&app);
                    true
                }
                (Key::v, true) => {
                    // Toggle the rendered preview.
                    let Some(w) = app.widgets.borrow().clone() else {
                        return Propagation::Proceed;
                    };
                    w.preview_toggle.set_active(!app.preview.get());
                    true
                }
                _ => false,
            };
            if handled {
                Propagation::Stop
            } else {
                Propagation::Proceed
            }
        });
    }
    window.add_controller(controller);
}

pub(crate) fn toggle_pin(app: &Rc<App>) {
    let Some(id) = app.open_note.borrow().clone() else {
        return;
    };
    let current = app.notes.get(&id).map(|n| n.is_pinned).unwrap_or(false);
    app.notes.set_pinned(&id, !current);
    refresh_all(app);
}

pub(crate) fn toggle_star(app: &Rc<App>) {
    let Some(id) = app.open_note.borrow().clone() else {
        return;
    };
    let current = app.notes.get(&id).map(|n| n.is_favorite).unwrap_or(false);
    app.notes.set_favorite(&id, !current);
    refresh_list(app);
}

pub(crate) fn delete_open(app: &Rc<App>) {
    let Some(id) = app.open_note.borrow().clone() else {
        return;
    };
    if matches!(*app.view.borrow(), View::Bin) {
        crate::dialogs::confirm_purge(app, &id);
    } else {
        crate::dialogs::confirm_delete(app, &id);
    }
}

// --- helpers -----------------------------------------------------------------

pub(crate) fn sort_order(app: &Rc<App>) -> SortOrder {
    app.prefs.get().sort_order()
}

/// The sort button's tooltip names the order it is *on*, not the action it
/// performs — a cycling button that only says "Sort order" makes the reader
/// press it twice to find out which of the three orders they are looking at.
pub(crate) fn sort_tooltip(order: SortOrder) -> String {
    match order {
        SortOrder::Edited => "Sort: recently edited (click for date created)".to_string(),
        SortOrder::Created => "Sort: date created (click for title)".to_string(),
        SortOrder::Title => "Sort: title (click for recently edited)".to_string(),
    }
}

/// The glyph the sort button wears for each order.
///
/// The two time orders get clocks, not the system's up-and-down arrows: the
/// list is sorted by *when*, and an arrow over lines says "sort" without
/// saying which way. "Recently edited" reads as the newer hand, "date
/// created" as the older one.
pub(crate) fn sort_icon(order: SortOrder) -> &'static str {
    match order {
        SortOrder::Edited => "view-sort-time-descending-symbolic",
        SortOrder::Created => "view-sort-time-ascending-symbolic",
        SortOrder::Title => "view-sort-alphabetical-symbolic",
    }
}
// ---------------------------------------------------------------------------
// Slash menu
// ---------------------------------------------------------------------------

/// Open the slash menu when `/` is typed on a line that holds nothing else.
///
/// The decision is `crate::slash::should_open`; this only measures the line and
/// builds the popover. The popover is owned by the text view (`set_parent`),
/// so it is freed with the editor rather than leaking one per keystroke.
pub(crate) fn install_slash_trigger(text_view: &gtk::TextView, app: &Rc<App>) {
    // Cloned so the controller's closure owns it: a `&TextView` cannot outlive
    // this function, and the controller lives as long as the editor does.
    let view = text_view.clone();
    let controller = gtk::EventControllerKey::new();
    {
        let app = app.clone();
        let view = view.clone();
        controller.connect_key_pressed(move |_, keyval, _, _| {
            if keyval != gdk::Key::slash {
                return Propagation::Proceed;
            }
            // The preview has no caret, and the bin is read-only.
            if app.preview.get() || matches!(*app.view.borrow(), View::Bin) {
                return Propagation::Proceed;
            }

            let buffer = view.buffer();
            let (text, sel) = crate::format::text_and_selection(&buffer);
            let line = crate::format::line_before_caret(&text, sel.ordered().0);
            if !crate::slash::should_open(&line) {
                return Propagation::Proceed;
            }

            show_slash_menu(&app, &view);
            // The `/` is left in the buffer: picking an item replaces it, and
            // dismissing the menu should leave the character the user typed.
            Propagation::Stop
        });
    }
    text_view.add_controller(controller);
}

pub(crate) fn show_slash_menu(app: &Rc<App>, text_view: &gtk::TextView) {
    let text_view = text_view.clone();
    let tv_for_pick = text_view.clone();
    let app_for_pick = app.clone();
    let popover = crate::slash::build_popover(&text_view, std::rc::Rc::new(move |item| {
        let Some(w) = app_for_pick.widgets.borrow().clone() else {
            return;
        };
        let buffer = tv_for_pick.buffer();
        let (text, sel) = crate::format::text_and_selection(&buffer);
        let edit = crate::slash::insertion_for(&text, sel, item);
        edit.apply(&buffer);
        tv_for_pick.grab_focus();
        // The menu may already be gone; `refresh_editor` only touches the
        // preview, and the caret belongs to the buffer.
        let _ = w;
    }));
    // A popover with no reference to hold it would be dropped the moment this
    // function returns, so the view keeps the only one it can.
    let _ = popover;
}