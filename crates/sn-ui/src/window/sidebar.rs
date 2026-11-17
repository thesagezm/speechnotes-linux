//! The sidebar and the notes-list/books-shelf rebuild paths (split out of
//! `window.rs`, Round 4, pure code-motion).

use std::rc::Rc;

use gtk4::prelude::*;
use gtk4 as gtk;

use sn_store::model::{Scope, SortOrder};
use sn_core::AppleDate;

use crate::app::{App, View};

use super::Widgets;
use super::editor::{refresh_editor, set_view};
use super::rows::{
    bin_row, book_card, book_row, empty_row, note_row, resize_book_cards, row_for,
    section_row, shelf_row_height, shelf_size_label,
};

// ---------------------------------------------------------------------------
// Rebuild paths
// ---------------------------------------------------------------------------

/// Rebuild everything: sidebar, list, editor. Use after a structural change.
pub fn refresh_all(app: &Rc<App>) {
    refresh_sidebar(app);
    refresh_list(app);
}

/// Rebuild the sidebar. Cheap: a handful of buttons.
pub fn refresh_sidebar(app: &Rc<App>) {
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    while let Some(child) = w.sidebar_box.first_child() {
        w.sidebar_box.remove(&child);
    }

    let view = app.view.borrow().clone();

    add_sidebar_item(
        app,
        &w,
        "All notes",
        "view-list-symbolic",
        View::Notes(Scope::All),
        &view,
    );
    add_sidebar_item(
        app,
        &w,
        "Unfiled",
        "folder-documents-symbolic",
        View::Notes(Scope::Unfiled),
        &view,
    );

    let label = gtk::Label::builder()
        .label("NOTEBOOKS")
        .halign(gtk::Align::Start)
        .css_classes(["sidebar-section"])
        .build();
    w.sidebar_box.append(&label);

    // The active set is read **once**: the counts used to call
    // `notes.active()` per notebook, and each call cloned the whole list.
    let active = app.active_notes();
    for notebook in app.notebooks.all() {
        let scope = Scope::Notebook(notebook.id.clone());
        let count = active
            .iter()
            .filter(|n| n.notebook_id.as_deref() == Some(notebook.id.as_str()))
            .count();
        let name = format!(
            "{}   {}",
            notebook.name,
            if count > 0 { count.to_string() } else { String::new() }
        );
        add_sidebar_item(app, &w, &name, "folder-symbolic", View::Notes(scope), &view);
    }

    // Books sits directly under the last notebook — a shelf is a peer of the
    // notebooks, and its position moves as notebooks are added, not fixed
    // below the bin (the bin stays last).
    let book_count =
        sn_store::books::library_cached(&app.library, &app.library_dirty)
            .map(|b| b.len())
            .unwrap_or(0);
    let label = gtk::Label::builder()
        .label("BOOKS")
        .halign(gtk::Align::Start)
        .css_classes(["sidebar-section"])
        .build();
    w.sidebar_box.append(&label);
    add_sidebar_item(
        app,
        &w,
        &format!("Library ({book_count})"),
        "audiobook-symbolic",
        View::Books,
        &view,
    );

    let stretch = gtk::Box::new(gtk4::Orientation::Vertical, 0);
    stretch.set_vexpand(true);
    w.sidebar_box.append(&stretch);

    // The Pomodoro card occupies the column's foot, above the Bin: the one
    // widget in the sidebar that is a tool rather than a destination. The
    // same card widget is re-appended on every rebuild — the timer and its
    // running clock live in the widget, not in the sidebar's layout.
    w.sidebar_box.append(&w.pomodoro.card);

    add_sidebar_item(
        app,
        &w,
        &format!("Bin ({})", app.notes.binned_active().len()),
        "user-trash-symbolic",
        View::Bin,
        &view,
    );
    
}

pub(crate) fn add_sidebar_item(
    app: &Rc<App>,
    w: &Widgets,
    label: &str,
    icon: &str,
    this_view: View,
    active_view: &View,
) {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    let image = gtk::Image::from_icon_name(icon);
    // The label ellipsizes *and* has a character cap.
    //
    // **Why both.** An ellipsized label still reports the width of its
    // whole text as its natural width, so "Pediatric Surgery" pinned the
    // sidebar's minimum width at the width of that name — the pane could
    // not shrink below it, and at a narrow window the whole layout
    // overflowed to the right (the sidebar's labels ended up at negative
    // x, clipped off the left edge). The cap is what lets the pane
    // shrink; the ellipsis is what keeps the name readable when it does.
    let text = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .label(label)
        .single_line_mode(true)
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        .max_width_chars(18)
        .build();
    row.append(&image);
    row.append(&text);

    let button = gtk::Button::builder().child(&row).css_classes(["flat"]).build();
    if &this_view == active_view {
        button.add_css_class("active");
    }
    button.set_hexpand(true);
    {
        let app = app.clone();
        button.connect_clicked(move |_| set_view(&app, this_view.clone()));
    }
    w.sidebar_box.append(&button);
}

/// Rebuild the notes list, memoised on the query key.
pub fn refresh_list(app: &Rc<App>) {
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };

    // Memoise: a version bump alone (a pin, a save) with an unchanged query
    // still needs a rebuild for the pin marker, so the key includes the
    // library version. The shelf's view toggles which tree fills, so it rides
    // in the key too; the column ceiling only re-lays-out, so it does not.
    //
    // The check comes **before** the query. The key carries every input the
    // query depends on — the search text, the notes version, the sort order,
    // the scope (in the view) — so a key that has not changed means the
    // query would return the same rows. The query used to run first, which
    // made the memo save nothing: every call cloned the whole note library,
    // folded it, filtered it and sorted it, and then threw the result away.
    // That is the path a keystroke takes, and it is why typing in the
    // search box was slow on a large library.
    let view = app.view.borrow().clone();
    let prefs = app.prefs.get();
    let mode = prefs.book_shelf_mode();
    let key = format!(
        "{view:?}|{}|{}|{:?}|{}|{}|{}|{}",
        app.search_text(),
        app.notes.version(),
        prefs.sort_order(),
        // A book import/delete changes the shelf but not the notes version.
        sn_store::books::library_cached(&app.library, &app.library_dirty)
            .map(|b| b.len())
            .unwrap_or(0),
        prefs.book_shelf_is_list(),
        mode,
        // The rows are built at the list's size step; a step change must
        // rebuild them, not memo-hit.
        prefs.book_list_size(),
    );
    let unchanged = *app.list_key.borrow() == key;
    let notes = if unchanged {
        // The rows are already on screen, and so is their number: the count
        // is kept from the last query that ran.
        Vec::new()
    } else {
        *app.list_key.borrow_mut() = key;
        app.visible_notes()
    };
    // The count the header shows, whether or not the query just ran.
    let count = if unchanged {
        app.list_count.get()
    } else {
        let count = notes.len();
        app.list_count.set(count);
        count
    };

    let header = match &view {
        View::Books => {
            let count = sn_store::books::library_cached(&app.library, &app.library_dirty)
                .map(|b| b.len())
                .unwrap_or(0);
            format!("BOOKS — {count}")
        }
        View::Bin => format!("BIN — {count}"),
        View::Notes(_) => {
            format!(
                "{} — {}",
                match prefs.sort_order() {
                    SortOrder::Edited => "Recently edited",
                    SortOrder::Created => "Date created",
                    SortOrder::Title => "Title",
                },
                count
            )
        }
    };
    w.list_header.set_text(&header);

    // The books view shows the shelf or the rows list, by pref; the other
    // two views always the list. One is visible at a time — the shelf is a
    // flow of fixed-width cards, the list is rows. The header row's quick
    // controls belong to the books view alone.
    let books_view = matches!(view, View::Books);
    let cards_mode = books_view && mode != "list";
    // The *scroll* wrapper is what the pane shows: hiding the FlowBox
    // alone would leave the wrapper (and its scrollbar) behind.
    w.shelf_scroll.set_visible(cards_mode);
    w.notes_scroll.set_visible(!cards_mode);
    w.shelf_controls.set_visible(books_view);
    w.shelf_size_button.set_visible(!prefs.book_shelf_is_list());
    // The tick callback reads its mode from here, in sync with the pref.
    app.shelf_mode.set(prefs.shelf_mode_ordinal() as u8);
    // Whatever set the pref (the cycle button, the settings page, a relaunch)
    // leaves the label stale; refresh_list is the one place every path goes
    // through, so the label follows the pref from here.
    shelf_size_label(app);

    if unchanged {
        // Nothing to rebuild: the rows on screen are the rows the key
        // describes.
        return;
    }

    let selected = app.open_note.borrow().clone();

    app.selection_suppress.set(true);
    while let Some(child) = w.notes_list.first_child() {
        w.notes_list.remove(&child);
    }
    while let Some(child) = w.book_shelf.first_child() {
        w.book_shelf.remove(&child);
    }
    *app.row_ids.borrow_mut() = Vec::new();

    // `row_ids` is indexed by visual row index, and `note_rows` gives one entry
    // per visual row — headers included — so the two cannot drift apart. A
    // header's id is the empty string, which can never match a note, so a click
    // on a section label opens nothing instead of opening a neighbour.
    let push = |app: &Rc<App>, row: gtk::ListBoxRow, id: &str| {
        app.row_ids.borrow_mut().push(id.to_string());
        w.notes_list.append(&row);
    };

    match &view {
        View::Notes(_) => {
            let now = AppleDate::now().raw();
            for entry in sn_store::query::note_rows(&notes, now) {
                let row = match &entry {
                    sn_store::query::Row::Header(section) => section_row(section.label()),
                    sn_store::query::Row::Note(note) => note_row(note),
                };
                push(app, row, entry.id());
            }
        }
        View::Bin => {
            if notes.is_empty() {
                push(app, empty_row("The bin is empty."), "");
            }
            for note in &notes {
                push(app, bin_row(note), &note.id);
            }
        }
        View::Books => {
            // Cards mode: books are covers, and the shelf is a flow of
            // fixed-width cards. List mode: the rows the shelf replaced —
            // the list's own selection handler opens them, because in this
            // view the row id IS a book id.
            let books = sn_store::books::library_cached(&app.library, &app.library_dirty)
                .unwrap_or_default();
            if cards_mode {
                if books.is_empty() {
                    let empty = gtk::Label::builder()
                        .label("No books yet — import one with the toolbar's book button.")
                        .wrap(true)
                        // The empty card sits on the same shelf, and the
                        // shelf's uniform cell is the largest natural card
                        // width — an uncapped wrapped label's natural is the
                        // whole sentence on one line, which would size every
                        // cell to it. The cap keeps the empty shelf's cards
                        // (this one, once a book lands) the same modest
                        // width the populated shelf wears.
                        .max_width_chars(24)
                        .halign(gtk::Align::Center)
                        .css_classes(["empty"])
                        .build();
                    let card = gtk::Box::builder()
                        .orientation(gtk::Orientation::Vertical)
                        .css_classes(["book-card"])
                        .build();
                    card.append(&empty);
                    w.book_shelf.append(&card);
                }
                for book in &books {
                    w.book_shelf.append(&book_card(app, book, mode));
                }
                // Fresh cards wear the static pre-measure shape; size them
                // for the mode now rather than waiting a frame — and this
                // is also what re-sizes after a mode switch, where the tick
                // callback's cached row height may already match. The
                // height is the *viewport's* (the shelf scrolls; its own
                // height is the cards' total).
                let height = w.shelf_scroll.height();
                if height > 1 {
                    resize_book_cards(&w.book_shelf, shelf_row_height(height, mode), mode);
                }
            } else {
                if books.is_empty() {
                    push(app, empty_row("No books yet — import one with the toolbar's book button."), "");
                }
                for book in &books {
                    push(app, book_row(app, book, app.prefs.get().book_list_size()), &book.manifest.id);
                }
            }
        }
    }

    // Restore selection without re-triggering open_note.
    if let Some(id) = selected {
        if let Some(row) = row_for(app, &id) {
            w.notes_list.select_row(Some(&row));
        }
    }
    app.selection_suppress.set(false);

    refresh_editor(app);
}

/// Switch the editor stack to the reader page, and the toolbar row with it.
pub fn show_reader_page(app: &Rc<App>) {
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    w.editor_stack.set_visible_child_name("reader");
    // Fixed 2026-11-02: the reader page takes the keyboard
    // focus. GTK4 dispatches a key event along the focused widget's
    // ancestor chain, and a window with no focused widget dispatches
    // none — so without this the reader's arrows did nothing until the
    // reader clicked an on-screen button (the click is what gave the
    // window a focus widget in the first place).
    super::focus::grab_reader_focus(app);
    // One toolbar row, two tool sets: the note tools stand down and the
    // reader's controls (prev/next, title, contents, mode, zoom, speak)
    // take the row. The note title entry has no book to name, so it goes
    // too — the book's title is in the row now.
    w.reader.controls.set_visible(true);
    // The open book's delete rides the reader's controls: visible exactly
    // when the reader page is, which is only ever true with a book open.
    w.reader.delete.set_visible(true);
    w.pin_button.set_visible(false);
    w.star_button.set_visible(false);
    w.bin_tools.set_visible(false);
    w.delete_button.set_visible(false);
    w.export_button.set_visible(false);
    w.image_button.set_visible(false);
    w.format_bar.set_visible(false);
    w.preview_toggle.set_visible(false);
    w.editor_title.set_visible(false);
}