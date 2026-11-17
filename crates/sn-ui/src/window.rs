//! Window construction and every signal.
//!
//! Widgets and their behaviour are not separable — a pane *is* the set of
//! closures that make it respond — so construction and wiring live together.
//! The rebuild path is the important part: **everything funnels through
//! [`refresh_list`] and [`refresh_editor`]**, and the list rebuild is memoised
//! on a query key, so a store version bump that does not change what is on
//! screen costs nothing.


use std::rc::Rc;

use gtk4::glib;
use gtk4::prelude::*;
use gtk4 as gtk;
use gtk4::glib::Propagation;

use crate::app::{App, View};

mod editor;
mod exporting;
mod focus;
mod rail;
mod rows;
mod shortcuts;
mod sidebar;
mod theme;

// The submodules are re-exported wholesale so every existing call path
// (`crate::window::refresh_all`, `crate::window::open_note`, ...) keeps
// resolving unchanged. Glob re-exports preserve each item's own visibility.
pub use editor::*;
pub(crate) use exporting::*;
pub(crate) use focus::*;
pub use rail::*;
pub(crate) use rows::*;
pub(crate) use shortcuts::*;
pub use sidebar::*;
pub use theme::*;

/// Widgets any signal needs to reach.
#[derive(Clone)]
pub struct Widgets {
    pub window: gtk::ApplicationWindow,
    pub sidebar_box: gtk::Box,
    pub notes_list: gtk::ListBox,
    /// The scroller around the notes list, hidden when the books shelf is up.
    pub notes_scroll: gtk::ScrolledWindow,
    /// The books shelf: a flow of cover cards, shown instead of the notes
    /// list when the books view is open. Books are covers; notes are a
    /// list. The flow reflows with the pane's width.
    pub book_shelf: gtk::FlowBox,
    pub shelf_scroll: gtk::ScrolledWindow,
    pub search: gtk::SearchEntry,
    pub sort_button: gtk::Button,
    pub new_note_button: gtk::Button,
    pub new_nb_button: gtk::Button,
    pub import_book_button: gtk::Button,
    pub settings_button: gtk::Button,
    /// Leave fill-the-window. Lives in the header's end beside the window
    /// controls, shown only while the mode is on — when every other button
    /// is hidden it is the one mouse route back.
    pub exit_focus: gtk::Button,
    pub editor_title: gtk::Entry,
    pub editor_text: gtk::TextView,
    pub editor_status: gtk::Label,
    pub editor_stack: gtk::Stack,
    pub editor_empty: gtk::Box,
    pub preview_box: gtk::Box,
    pub preview_toggle: gtk::ToggleButton,
    /// Expand-to-window. The one toggle left: the reader's controls moved
    /// into this toolbar, so the book page and the note page share it.
    pub focus_toggle: gtk::ToggleButton,
    /// The single toolbar row: the note tools and the reader's controls
    /// (`Widgets::reader.controls`) packed into one box, one set visible at
    /// a time. Focus mode hides the row whole.
    ///
    /// This is the *scroller* around the row, not the row itself: a narrow
    /// window scrolls the surplus buttons sideways rather than clipping
    /// them, so the reader's own speak button never simply disappears.
    pub toolbar: gtk::ScrolledWindow,
    /// (root, list/editor) panes — what focus mode hides the start children
    /// of, and what the saved positions restore.
    pub panes: (gtk::Paned, gtk::Paned),
    /// The focus-mode slides: the two dividers that move out of the way
    /// when the page takes the whole window, and back when it leaves.
    pub slides: (crate::motion::Slide, crate::motion::Slide),
    /// The per-column rails: a slim chevron at the left edge of the
    /// list/editor column (the sidebar's) and at the left edge of the editor
    /// (the notes list's). Each lives in the *end* child of its paned, so it
    /// stays on screen when the column it toggles is collapsed — one control
    /// collapses and expands, and a column is never recoverable only through
    /// the paned's own few-pixel handle.
    pub sidebar_rail: gtk::Button,
    pub list_rail: gtk::Button,
    /// The animation that fades the chrome in and out with the mode.
    pub focus_anim: crate::motion::Anim,
    /// The formatting bar's row — the scroller around it, so focus mode
    /// hides the whole strip and a narrow window scrolls the surplus
    /// buttons sideways.
    pub format_bar: gtk::ScrolledWindow,
    pub pin_button: gtk::ToggleButton,
    pub star_button: gtk::ToggleButton,
    pub delete_button: gtk::Button,
    pub export_button: gtk::Button,
    pub image_button: gtk::Button,
    pub bin_tools: gtk::Box,
    pub list_header: gtk::Label,
    /// The books view's quick controls, riding the list header row: the
    /// list/cards pair and the shelf's column ceiling. Visible only in the
    /// books view — the notes views have no shelf to tune.
    pub shelf_controls: gtk::Box,
    pub shelf_columns: gtk::DropDown,
    pub shelf_size_button: gtk::Button,
    pub player: crate::player::PlayerWidgets,
    /// The scroller around the transport row. The row itself is wide, so it
    /// scrolls sideways rather than setting the window's minimum width.
    /// Focus mode hides *this*, not the row inside it — hiding the row
    /// alone would leave an empty strip where the transport was.
    pub player_scroll: gtk::ScrolledWindow,
    /// The engine/voice pickers whole — the speech-language change in
    /// settings refreshes the rail through `picker::refresh`, which needs
    /// the pair (and its guard), not just the combos.
    pub picker: crate::picker::PickerWidgets,
    pub reader: crate::reader::ReaderWidgets,
    /// The Pomodoro card and its banner. The card is re-parented into the
    /// sidebar on every `refresh_sidebar`; the banner is an overlay child
    /// over the page area, so a finished phase is announced in
    /// fill-the-window mode too.
    pub pomodoro: crate::pomodoro::PomodoroWidgets,
}

/// Entry point: build everything and hand control to GTK.
pub fn build_window(gio_app: &gtk::Application, app: &Rc<App>) {
    let window = gtk::ApplicationWindow::builder()
        .application(gio_app)
        .title("Speechnotes")
        .default_width(1180)
        .default_height(740)
        .build();

    // ---- header bar ------------------------------------------------------
    let header = gtk::HeaderBar::builder().show_title_buttons(true).build();

    // The search entry's minimum width. 240 px was enough to type into but
    // wide enough that a narrow window's header bar spent a third of its
    // space on the box rather than on the buttons. 140 px still shows a
    // word or two, and the entry expands into whatever is left over.
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search notes")
        .hexpand(true)
        .width_request(140)
        .build();
    {
        let app = app.clone();
        search.connect_activate(move |_| {
            if matches!(*app.view.borrow(), View::Books) {
                if let Some(chapter) = app.reader.borrow().search_first_hit {
                    if let Some(id) = app.reader.borrow().book_id.clone() {
                        crate::reader::open_book(&app, &id, chapter);
                    }
                }
            }
        });
    }
    {
        let app = app.clone();
        search.connect_search_changed(move |_| {
            // In the books view the search entry searches the open book, not
            // the notes index: one entry, the domain the view names.
            if matches!(*app.view.borrow(), View::Books) {
                crate::reader::search_current_book(&app, &app.search_text());
            }
            refresh_list(&app);
        });
    }
    header.set_title_widget(Some(&search));

    let sort_button = gtk::Button::from_icon_name(sort_icon(sort_order(&app)));
    sort_button.set_tooltip_text(Some(&sort_tooltip(sort_order(&app))));
    {
        let app = app.clone();
        let sb = sort_button.clone();
        sort_button.connect_clicked(move |_| {
            let next = sort_order(&app).next();
            app.prefs.update(|p| p.set_sort_order(next));
            sb.set_icon_name(sort_icon(next));
            sb.set_tooltip_text(Some(&sort_tooltip(next)));
            refresh_list(&app);
        });
    }
    header.pack_start(&sort_button);

    let new_note_button = gtk::Button::from_icon_name("document-new-symbolic");
    new_note_button.set_tooltip_text(Some("New note (Ctrl+N)"));
    {
        let app = app.clone();
        new_note_button.connect_clicked(move |_| new_note(&app));
    }
    header.pack_start(&new_note_button);

    // Leave fill-the-window. Packed first among the header's end children so
    // it lands against the window controls — in focus mode it is, with them,
    // the only thing on the bar.
    let exit_focus = gtk::Button::from_icon_name("view-restore-symbolic");
    exit_focus.set_tooltip_text(Some("Leave fill-the-window (F11)"));
    {
        let app = app.clone();
        exit_focus.connect_clicked(move |_| apply_focus_mode(&app, false));
    }
    exit_focus.set_visible(false);
    header.pack_end(&exit_focus);

    let new_nb_button = gtk::Button::from_icon_name("folder-new-symbolic");
    new_nb_button.set_tooltip_text(Some("New notebook (Ctrl+Shift+N)"));
    {
        let app = app.clone();
        new_nb_button.connect_clicked(move |_| crate::dialogs::new_notebook(&app));
    }
    header.pack_end(&new_nb_button);

    // The shelf's import button used to wear the audiobook glyph — a button
    // that imports EPUBs, PDFs and slide decks alike said "audiobook". It
    // gets its own open-book-with-arrow face now.
    let import_book_button = gtk::Button::from_icon_name("book-import-symbolic");
    import_book_button.set_tooltip_text(Some(
        "Import a book (EPUB, PDF, Kindle, FB2, RTF, HTML or text)",
    ));
    {
        let app = app.clone();
        import_book_button.connect_clicked(move |_| crate::reader::pick_and_import(&app));
    }
    header.pack_end(&import_book_button);

    let settings_button = gtk::Button::from_icon_name("emblem-system-symbolic");
    settings_button.set_tooltip_text(Some("Settings"));
    {
        let app = app.clone();
        settings_button.connect_clicked(move |_| crate::settings::show(&app));
    }
    header.pack_end(&settings_button);

    window.set_titlebar(Some(&header));

    // ---- the three columns ------------------------------------------------
    let sidebar = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .css_classes(["sidebar"])
        .margin_top(10)
        .margin_bottom(10)
        .margin_start(6)
        .margin_end(6)
        .build();

    let list_header = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .css_classes(["section-label"])
        .label("NOTES")
        .build();

    // The books view's quick controls ride the header row: the list/cards
    // pair, and — in cards mode — the shelf's column ceiling. The row is a
    // header, not a scroller, so the controls cannot scroll away on a narrow
    // window. Initial state comes from the prefs and is set *before* the
    // handlers connect, so no handler fires for the state it just read.
    let shelf_list_toggle = gtk::ToggleButton::builder()
        .icon_name("view-list-symbolic")
        .tooltip_text("Show the books as a list")
        .css_classes(["flat"])
        .build();
    let shelf_cards_toggle = gtk::ToggleButton::builder()
        .icon_name("view-grid-symbolic")
        .tooltip_text("Show the books as cards")
        .css_classes(["flat"])
        .build();
    shelf_cards_toggle.set_group(Some(&shelf_list_toggle));
    // The card-size cycle: one flat button whose *label is the current
    // mode* — Small / Tiles / Large / Huge — so the name always says what
    // is showing (and what a click changes), which is what makes it
    // drivable from outside. List mode hides it: sizes mean nothing there.
    let shelf_size_button = gtk::Button::with_label("Tiles");
    shelf_size_button.set_tooltip_text(Some("Card size (click to cycle: Small, Tiles, Large, Huge)"));
    shelf_size_button.set_css_classes(&["flat"]);
    let shelf_columns = gtk::DropDown::from_strings(&[
        "1 column", "2 columns", "3 columns", "4 columns", "5 columns", "6 columns",
    ]);
    crate::a11y::stable_dropdown_label(&shelf_columns, "The most card columns the shelf may use");
    // The dropdown's popup cannot be operated through AT-SPI, so the
    // choice would be readable but not reachable from outside. The
    // steppers make it drivable: the dropdown stays the single source of
    // truth, and the arrows change exactly what picking a row would.
    let (shelf_columns_down, shelf_columns_up) =
        crate::a11y::dropdown_steppers(&shelf_columns);
    let shelf_controls = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .valign(gtk::Align::Center)
        .build();
    shelf_controls.append(&shelf_list_toggle);
    shelf_controls.append(&shelf_cards_toggle);
    shelf_controls.append(&shelf_size_button);
    shelf_controls.append(&shelf_columns_down);
    shelf_controls.append(&shelf_columns);
    shelf_controls.append(&shelf_columns_up);
    {
        let prefs = app.prefs.get();
        if prefs.book_shelf_is_list() {
            shelf_list_toggle.set_active(true);
        } else {
            shelf_cards_toggle.set_active(true);
        }
        shelf_size_button.set_label(SHELF_MODES[prefs.shelf_mode_ordinal()].1);
        shelf_columns.set_selected(prefs.book_shelf_columns() - 1);
    }
    {
        // The cycle click: next mode, persisted, label updated, shelf
        // rebuilt (refresh_list's key carries the mode, so this rebuilds).
        let app = app.clone();
        let size_button = shelf_size_button.clone();
        shelf_size_button.connect_clicked(move |_| {
            let next = (app.prefs.get().shelf_mode_ordinal() + 1) % SHELF_MODES.len();
            app.prefs.update(|p| p.book_shelf_view = SHELF_MODES[next].0.to_string());
            size_button.set_label(SHELF_MODES[next].1);
            crate::window::refresh_list(&app);
        });
    }
    let list_header_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .build();
    list_header_row.append(&list_header);
    let header_stretch = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    header_stretch.set_hexpand(true);
    list_header_row.append(&header_stretch);
    list_header_row.append(&shelf_controls);

    // The shelf's header row scrolls sideways when the pane is narrow —
    // the toolbar's own pattern, for the same reason. **Why.** The row is
    // the section label plus the shelf controls (the list/cards pair, the
    // size cycle and the columns dropdown with its steppers): ~470 px of
    // natural width. The `list_editor` Paned's divider can sit at 400 px,
    // and a plain Box makes that 470 the *pane's minimum* — GTK paints the
    // overflow past the divider, to the left, under the sidebar: the
    // "BOOKS — N" header and the first card column half-hidden at a 984 px
    // window, clean at 1942 px. Inside a scroller with the natural width
    // not propagated, the row keeps its width and the *pane* stays at the
    // divider; what does not fit scrolls instead of overlaying its
    // neighbours. (The hexpand spacer is safe here: its natural width is
    // zero, so it contributes nothing to the scroller's range — the same
    // argument as the toolbar's spacer above.)
    let list_header_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_width(false)
        .css_classes(["toolbar-scroll"])
        .child(&list_header_row)
        .build();
    // The scrollbar must overlay. This scroller's height is the header row's
    // own (34 px, the controls' height); a space-taking horizontal scrollbar
    // steals 15 px of that viewport and paints the row's bottom half under
    // the shelf — measured live: a 19 px viewport under a 400×15 scrollbar,
    // every header control cut mid-height. Overlay leaves the full 34 px to
    // the row; the `.toolbar-scroll scrollbar` rule holds the floating bar
    // to a 3 px line for the rare moments it shows.
    list_header_scroll.set_overlay_scrolling(true);

    let notes_list = gtk::ListBox::builder()
        .css_classes(["notes-list"])
        .selection_mode(gtk::SelectionMode::Single)
        .build();
    {
        let app = app.clone();
        notes_list.connect_row_selected(move |_, row| {
            if app.selection_suppress.get() {
                return;
            }
            let Some(row) = row else {
                return;
            };
            if let Some(id) = row_data(&app, row) {
                // In the books view the row's id is a book id, and the pane
                // it opens is the reader — never the note editor, which
                // would show the note with that id (there is none). RESUME
                // is the book's own saved position: the bookmark's chapter
                // and, for an audiobook, its millisecond offset — computing
                // the chapter here and passing it explicitly was what made
                // a book saved mid-chapter reopen that chapter at 0:00.
                if matches!(*app.view.borrow(), View::Books) {
                    crate::reader::open_book(&app, &id, crate::reader::RESUME);
                    return;
                }
                open_note(&app, &id);
            }
        });
    }

    let list_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&notes_list)
        .build();

    // The books shelf: a flow of fixed-width cover cards. Books are covers —
    // a shelf that is a list of rows throws away the one thing a book has
    // that a note does not. The cards reflow, so a narrow pane shows one
    // card and a wide one as many as the user's ceiling allows — the cards
    // themselves never stretch with the pane.
    //
    // The two alignments ARE the fix for the two ways the cards used to
    // grow wrong. A homogeneous FlowBox whose own halign/valign is the
    // default Fill divides the pane evenly: every row gets `height / rows`
    // and every column `width / columns`, and a Fill-aligned card fills its
    // cell. With a single row of books that cell was the whole pane — the
    // one row of cards stretched to the bottom of the shelf (two rows
    // shared the extra and only mostly normalised). Centre and Start size
    // every row and column to the cards' largest natural size instead and
    // park the leftover at the edge; the card fills that uniform cell, so
    // every card wears the same width regardless of its title. The FlowBox
    // itself still fills the viewport (vexpand), so the shelf background
    // paints the whole pane — only the cells stop stretching.
    let book_shelf = gtk::FlowBox::builder()
        .css_classes(["book-shelf"])
        .selection_mode(gtk::SelectionMode::None)
        .homogeneous(true)
        .column_spacing(4)
        .row_spacing(4)
        .min_children_per_line(1)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Start)
        .vexpand(true)
        .build();
    book_shelf.set_max_children_per_line(app.prefs.get().book_shelf_columns());

    // The shelf scrolls. The FlowBox is its content, not its own viewport:
    // bare in the pane it handed its whole content height to the window as
    // a minimum (eleven cards ≈ 9600 px), GDK clamped the window to the
    // screen, and everything below the fold — the transport bar, the
    // sidebar's bottom half, both rail chevrons (their strip stretches with
    // the box, centre ~4700 px down) — painted off-window. The notes list
    // has always worn the same wrapper; the shelf does now too.
    let shelf_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&book_shelf)
        .build();

    // The cards' size is a **fraction of the shelf, not a constant**. GTK's
    // CSS has no percentage lengths, so the fraction is measured in Rust: a
    // tick callback (the same mechanism the web viewers use) reads the
    // shelf's real allocation every frame it is on screen and, when the
    // derived row height moves, re-sizes every card. Six rows fill a
    // full-height window; as the window loses height the row floor holds and
    // the shelf fits fewer rows instead of shrinking its cards away — the
    // orderly decrease the brief asked for. Width follows the cover's own
    // aspect, so nothing stretches at any size. The fraction reads the
    // *scrolling pane's* height — the FlowBox inside it is the content, and
    // its own height is the cards' total, not the pane's.
    let shelf_row = std::cell::Cell::new(0i32);
    {
        let app = app.clone();
        let pane_height = shelf_scroll.clone();
        book_shelf.add_tick_callback(move |shelf, _clock| {
            let height = pane_height.height();
            if height > 1 {
                // The mode lives in a cell `refresh_list` keeps in sync —
                // the tick runs per frame and must not clone the prefs.
                let mode = SHELF_MODES[app.shelf_mode.get().min(3) as usize].0;
                let row = shelf_row_height(height, mode);
                if row != shelf_row.get() {
                    shelf_row.set(row);
                    resize_book_cards(shelf, row, mode);
                }
            }
            glib::ControlFlow::Continue
        });
    }

    let list_pane = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(0)
        .build();
    list_pane.append(&list_header_scroll);
    list_pane.append(&list_scroll);
    list_pane.append(&shelf_scroll);

    // ---- editor pane -------------------------------------------------------
    //  hexpand authored — WebKit reports the deck page's current
    // width as the webview's minimum AND natural, so in a plain Box the
    // editor pane collapsed to whatever the page was last fitted to (a
    // full-width pane after fill-the-window shrank to one slide). Expand
    // explicitly; the pane's width is the layout's decision, not the
    // document's.
    let editor_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(0)
        .hexpand(true)
        .build();

    // The toolbar row scrolls sideways when the window is narrow.
    //
    // **Why.** The row holds the note tools *and* the reader's controls —
    // chapter steppers, the title, the contents, the mode toggle, the zoom
    // family, the speak button and the status. On a narrow window that is
    // more buttons than there is room for, and without a scroller the last
    // ones are clipped and simply gone: the reader's own speak button was
    // unreachable on a small window, with no way to discover that.
    //
    // **How.** A `ScrolledWindow` with `propagate_natural_width(false)` —
    // the scroller does not grow the window to fit its content, so the
    // toolbar keeps its natural width and the *window* stays narrow. The
    // toolbar's own hexpand spacer is dropped inside a scroller (a spacer
    // that expands to fill a scroller's viewport is meaningless, and it
    // would push the delete button off the end); the row's right-hand
    // buttons are packed at the end instead.
    let toolbar = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(4)
        .css_classes(["toolbar"])
        .build();
    let pin_button = gtk::ToggleButton::new();
    pin_button.set_icon_name("view-pin-symbolic");
    pin_button.set_tooltip_text(Some("Pin (Ctrl+Shift+P)"));
    let star_button = gtk::ToggleButton::new();
    star_button.set_icon_name("starred-symbolic");
    star_button.set_tooltip_text(Some("Favourite (Ctrl+Shift+S)"));
    let delete_button = gtk::Button::new();
    delete_button.set_icon_name("user-trash-symbolic");
    delete_button.set_tooltip_text(Some("Move to bin (Ctrl+Shift+D)"));
    toolbar.append(&pin_button);
    toolbar.append(&star_button);

    let bin_tools = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(4)
        .visible(false)
        .build();
    let restore_button = gtk::Button::with_label("Recover");
    let purge_button = gtk::Button::with_label("Delete forever");
    purge_button.add_css_class("destructive-action");
    bin_tools.append(&restore_button);
    bin_tools.append(&purge_button);
    toolbar.append(&bin_tools);

    // The delete button sits at the row's end, with a spacer before it so
    // it stays at the right edge of a wide window.
    //
    // The spacer is hexpand, and that is safe inside a scroller: a spacer's
    // *natural* width is zero, so it contributes nothing to the width the
    // scroller measures. When the row fits, the spacer absorbs the slack
    // and the delete button is at the right edge; when the row does not
    // fit, the spacer has no slack to take, collapses to nothing, and the
    // scroller's range is exactly the buttons' own width — so the delete
    // button is the last thing the reader scrolls to, not the first thing
    // that disappears.
    let toolbar_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    toolbar_spacer.set_hexpand(true);
    toolbar.append(&toolbar_spacer);
    toolbar.append(&delete_button);

    // The scroller around the row. The policy is `Automatic` so the
    // scrollbar appears only when the row really does not fit, and the
    // natural width is *not* propagated, so the row's width does not push
    // the window wider than the user asked for.
    //
    // **Overlay scrolling is off.** GTK4's default is an *overlay*
    // scrollbar: it floats over the content and hides itself until the
    // user scrolls, so a row that does not fit looks exactly like a row
    // whose last buttons do not exist. That was the bug the last gate
    // found — the export, image, preview and fill-window buttons sat past
    // the right edge with nothing to say they were there. A scrollbar that
    // is always there when there is something to scroll to is the
    // affordance that says "there is more this way".
    let toolbar_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_width(false)
        .css_classes(["toolbar-scroll"])
        .child(&toolbar)
        .build();
    toolbar_scroll.set_overlay_scrolling(false);
    editor_box.append(&toolbar_scroll);

    // ---- formatting bar ----------------------------------------------------
    // Sits under the note toolbar and acts on the real buffer and the real
    // caret. Every transform is a pure function in `crate::format`; this only
    // reads the selection, calls it, and writes the result back.
    let format_bar = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(2)
        .css_classes(["format-bar"])
        .build();
    // Every name here is checked by `cargo run -p sn-ui --example check_icons`,
    // which asks the live icon theme about each one. Seven of them come from the
    // bundled theme in `icons.rs`; the rest are Adwaita names Cosmic also ships.
    for (icon, tip, kind) in [
        ("format-text-bold-symbolic", "Bold (Ctrl+B)", FormatAction::Wrap("**")),
        ("format-text-italic-symbolic", "Italic (Ctrl+I)", FormatAction::Wrap("*")),
        ("format-text-strikethrough-symbolic", "Strikethrough", FormatAction::Wrap("~~")),
        ("format-text-backslash-symbolic", "Code (Ctrl+`)", FormatAction::Wrap("`")),
        (
            "format-text-heading-symbolic",
            "Heading (Ctrl+Alt+1)",
            FormatAction::Line("# "),
        ),
        (
            "view-list-symbolic",
            "Bullet list",
            FormatAction::Line("- "),
        ),
        (
            "format-text-bulleted-list-symbolic",
            "Numbered list",
            FormatAction::Line("1. "),
        ),
        ("format-text-quote-symbolic", "Quote", FormatAction::Line("> ")),
        ("view-grid-symbolic", "Insert table", FormatAction::Table),
        ("insert-link-symbolic", "Link (Ctrl+K)", FormatAction::Link),
    ] {
        let button = gtk::Button::from_icon_name(icon);
        button.set_tooltip_text(Some(tip));
        button.add_css_class("flat");
        {
            let app = app.clone();
            button.connect_clicked(move |_| {
                apply_format_action(&app, &kind);
            });
        }
        format_bar.append(&button);
    }
    // Ten buttons in a row: on a narrow window the last of them would be
    // clipped, so the row scrolls sideways instead. Same shape as the
    // toolbar's scroller, and for the same reason — a control that is
    // clipped away is a control that does not exist.
    let format_bar_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_width(false)
        .css_classes(["format-bar-scroll"])
        .child(&format_bar)
        .build();
    // Overlay scrolling off, for the same reason as the note toolbar's:
    // a hidden scrollbar makes the row's last buttons look absent.
    format_bar_scroll.set_overlay_scrolling(false);
    editor_box.append(&format_bar_scroll);

    let editor_title = gtk::Entry::builder()
        .css_classes(["title-entry"])
        .placeholder_text("Title")
        .build();
    {
        let app = app.clone();
        editor_title.connect_changed(move |_| {
            if app.editor_suppress.get() {
                return;
            }
            let Some(id) = app.open_note.borrow().clone() else {
                return;
            };
            // Record intent only. Committing — and the list rebuild that
            // follows — happens once, when the debounce fires; rebuilding the
            // ListBox per title keystroke is the expensive way to spell.
            *app.pending_title.borrow_mut() = Some(id);
            arm_commit_timer(&app);
        });
    }
    editor_box.append(&editor_title);

    let editor_text = gtk::TextView::builder()
        .css_classes(["editor-text"])
        .wrap_mode(gtk::WrapMode::Word)
        .top_margin(10)
        .bottom_margin(24)
        .left_margin(14)
        .right_margin(14)
        .build();
    // Right-click → "Speak from here": the voice picks up from the cursor
    // or the selection's first character.
    crate::player::attach_speak_from_here(app, &editor_text, false);
    //  the selection's own pill beside it, and F5
    // through the same offset logic (window/shortcuts.rs).
    crate::speaksel::attach_selection_affordance(app, &editor_text, false);
    // The text view goes to the stack *through* a scroller. A bare TextView
    // outside a ScrolledWindow has nothing driving its adjustments, so a note
    // longer than the window simply clipped at the bottom with no scrollbar and
    // no way to reach the last line — the bug the user reported first.
    let editor_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&editor_text)
        .build();
    {
        let app = app.clone();
        editor_text
            .buffer()
            .connect_changed(move |buf| buffer_changed(&app, buf));
    }
    install_slash_trigger(&editor_text, app);

    let editor_empty = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .valign(gtk::Align::Center)
        .halign(gtk::Align::Center)
        .spacing(6)
        .vexpand(true)
        .build();
    let empty_title = gtk::Label::new(None);
    empty_title.set_markup(
        "<span size=\"x-large\" weight=\"bold\">No note selected</span>",
    );
    let empty_hint = gtk::Label::new(Some(
        "Choose a note on the left, or press Ctrl+N to write one.",
    ));
    // The empty state's labels wrap and ellipsize, and the box does not
    // demand a width of its own.
    //
    // **Why this matters.** A `GtkLabel`'s *natural* width is the width of
    // its whole text on one line, and the empty state's hint is 49
    // characters. That single label made the editor pane's minimum width
    // 337 px — and because a `GtkPaned` does not shrink a child below that
    // minimum unless told to, the pane *overflowed* the window instead of
    // compressing: at a 955 px window the editor pane's right edge landed
    // at x=974, and at 729 px the sidebar was pushed to x=-43 with its
    // labels clipped off the left edge.
    //
    // So the labels wrap (the text is short enough to wrap over two lines
    // in a narrow pane), ellipsize as a last resort, and the box's own
    // minimum is dropped to nothing. The pane can now be squeezed to the
    // width the window actually has.
    for label in [&empty_title, &empty_hint] {
        label.set_wrap(true);
        label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_max_width_chars(32);
        label.set_width_request(0);
    }
    editor_empty.set_width_request(0);
    empty_title.add_css_class("empty");
    empty_hint.add_css_class("empty");
    editor_empty.append(&empty_title);
    editor_empty.append(&empty_hint);

    // The rendered preview. A plain box whose single child is replaced on each
    // render, so the stack owns the page and the renderer owns the contents.
    let preview_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .vexpand(true)
        .build();
    //  a selection in a preview label is a passage
    // the voice can start from — right-click offers it (F5 reaches the same
    // through speaksel::speak_from_selection).
    crate::speaksel::attach_preview_speak(app, &preview_box);

    let reader = crate::reader::build(app);
    let pomodoro = crate::pomodoro::build(app);
    // The reader's controls join the note tools in the one toolbar row: the
    // note set hides while a book is open (`show_reader_page`) and comes
    // back with the note (`refresh_editor`). Prepended, so the note tools
    // keep their places at the row's end.
    toolbar.prepend(&reader.controls);
    reader.controls.set_visible(false);

    // The editor's pages crossfade. A hard cut between the editor, the
    // preview and the reader is a flash — the reader's chrome appears in a
    // different place than the note's did, and the eye has to find it
    // again. A 240 ms crossfade says "this is the same page, showing
    // something else", and GTK does the work: the stack draws both children
    // for the length of the fade, which is two widgets over one area — not
    // a per-frame redraw of the whole window.
    let editor_stack = gtk::Stack::builder()
        .vexpand(true)
        .transition_type(gtk::StackTransitionType::Crossfade)
        .transition_duration(crate::motion::FADE_MS)
        .build();
    editor_stack.add_titled(&editor_empty, Some("empty"), "No note");
    editor_stack.add_titled(&editor_scroll, Some("editor"), "Editor");
    editor_stack.add_titled(&preview_box, Some("preview"), "Preview");
    // The book reader owns the pane while a book is open: it has its own
    // toolbar, its own scroll and its own speech path, so it is a page of
    // the stack rather than a mode of the note editor.
    editor_stack.add_titled(&reader.box_, Some("reader"), "Reader");

    // Everything that floats over the open page hangs off one overlay: the
    // reader's fill-the-window transport pill, and the Pomodoro's gentle
    // banner — which must be visible in fill-the-window mode too, when the
    // reader's eyes are on the book and the phase just ended.
    let page_overlay = gtk::Overlay::builder().vexpand(true).build();
    page_overlay.set_child(Some(&editor_stack));
    page_overlay.add_overlay(&reader.float);
    page_overlay.add_overlay(&pomodoro.banner);
    {
        // Mouse motion over the page is what summons the transport pill in
        // fill-the-window mode; `float_poke` does nothing outside it.
        let app = app.clone();
        let motion = gtk4::EventControllerMotion::new();
        motion.connect_motion(move |_, _, _| crate::reader::float_poke(&app));
        page_overlay.add_controller(motion);
    }
    editor_box.append(&page_overlay);

    // A toggle rather than a permanent split: a note is written far more often
    // than it is read back, and a permanent preview would cost half the editor
    // width for the whole session.
    let export_button = gtk::Button::new();
    export_button.set_icon_name("document-save-symbolic");
    export_button.set_tooltip_text(Some("Export this note (Ctrl+Shift+E)"));
    toolbar.append(&export_button);

    let image_button = gtk::Button::new();
    image_button.set_icon_name("image-insert-symbolic");
    image_button.set_tooltip_text(Some("Insert an image (Ctrl+Shift+I)"));
    toolbar.append(&image_button);

    let preview_toggle = gtk::ToggleButton::new();
    preview_toggle.set_icon_name("view-preview-symbolic");
    preview_toggle.set_tooltip_text(Some("Preview (Ctrl+Shift+P is pin; this is Ctrl+E)"));
    toolbar.append(&preview_toggle);

    // Expand to the whole window. The one toggle there is — the reader's
    // controls live in this toolbar too, so the book page and the note page
    // share it; F11 is the keyboard route, and the header's leave button is
    // the route while the mode is on. See `apply_focus_mode`.
    let focus_toggle = gtk4::ToggleButton::builder()
        .icon_name("view-fullscreen-symbolic")
        .build();
    focus_toggle.set_tooltip_text(Some("Fill the whole window (F11)"));
    toolbar.append(&focus_toggle);

    let editor_status = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .css_classes(["status"])
        .build();
    editor_box.append(&editor_status);

    // ---- assemble ----------------------------------------------------------
    //
    // **Why the paned's children may shrink.** A `GtkPaned` gives each
    // child its requested width and lets the *other* child overflow when
    // the window is too narrow — the layout then spills past the window's
    // edge, which puts the toolbar's scrollbar off-screen (the buttons
    // look absent) and pushes the sidebar to negative x (its labels are
    // clipped off the left). With `shrink-start-child` and
    // `shrink-end-child` on, a pane that cannot have its requested width
    // gives some of it back, and the whole layout stays inside the
    // window.
    //
    // The children themselves have small minimums (the editor pane's is
    // 56 px, the sidebar's 209), so shrinking them costs little: the
    // editor's *contents* scroll, and the sidebar's labels ellipsize.
    let list_editor = gtk::Paned::builder()
        .orientation(gtk::Orientation::Horizontal)
        .position(400)
        .shrink_start_child(true)
        .shrink_end_child(true)
        .build();
    list_editor.set_start_child(Some(&list_pane));
    // The notes list's rail rides at the editor's left edge — inside the end
    // child, so it survives the list being collapsed.
    let list_rail = rail_button("Hide the notes list");
    // Fixed 2026-11-02: the rail stands **clear of the
    // divider**. This rail is the end child's *first* child, so it sits
    // against the paned's handle — the handle's drag gesture claims the
    // pointer there, and a click that lands on it resizes the column
    // instead of pressing the button, which moves the rail out from under
    // the pointer (the user's "I have to click it five times"). The
    // margin puts the button's own pixels clear of the handle, in this
    // state and in the collapsed one (where the rail rides against the
    // *root* paned's boundary instead).
    list_rail.set_margin_start(10);
    let editor_wrap = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    editor_wrap.append(&list_rail);
    editor_wrap.append(&editor_box);
    list_editor.set_end_child(Some(&editor_wrap));

    let root = gtk::Paned::builder()
        .orientation(gtk::Orientation::Horizontal)
        .position(235)
        .shrink_start_child(true)
        .shrink_end_child(true)
        .hexpand(true) // see editor_box — same document-width collapse
        .build();
    root.set_start_child(Some(&sidebar));
    root.set_end_child(Some(&list_editor));

    // The sidebar's rail rides at the left edge of everything the sidebar
    // leaves behind — the same survival rule, one level up.
    let sidebar_rail = rail_button("Hide the sidebar");
    let columns = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    columns.append(&sidebar_rail);
    columns.append(&root);

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
    vbox.append(&columns);
    // The transport sits under everything: playback is global, not part of
    // the list/editor split. The engine/voice pickers ride in the same row —
    // one bar, not a bar and a half; they drive the same session the buttons
    // do, and they read the same prefs `--speak` does.
    let player = crate::player::build(app);
    let picker = crate::picker::build(app);
    player.bar.append(&picker.row);
    // The transport row scrolls sideways rather than setting the window's
    // minimum width.
    //
    // **Why.** The row's contents add up to 683 px — three transport
    // buttons, the progress bar, the speed dial and the engine/voice
    // pickers — and a plain box in a vertical box hands that total to the
    // window as a minimum. The window then refused to go below 693 px, and
    // on a narrower screen the *whole layout* overflowed to the right
    // instead of compressing: the editor pane's right edge landed past the
    // window's, which pushed the note toolbar's scrollbar off-screen
    // (the buttons looked unreachable) and clipped the sidebar to negative
    // x.
    //
    // A `ScrolledWindow` with `propagate_natural_width(false)` gives the
    // row its own horizontal scroll instead: the window may be as narrow as
    // the user likes, and the row's surplus scrolls. The scrollbar is an
    // overlay one, so the row's height never changes when it appears.
    let player_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_width(false)
        .css_classes(["transport-scroll"])
        .child(&player.bar)
        .build();
    // Overlay scrolling off: the transport row's surplus (the engine and
    // voice pickers) must be *findable* when the window is narrow.
    player_scroll.set_overlay_scrolling(false);
    vbox.append(&player_scroll);
    window.set_child(Some(&vbox));

    // ---- signals that need the finished widget set -------------------------
    {
        let app = app.clone();
        pin_button.connect_toggled(move |btn| {
            if app.editor_suppress.get() {
                return;
            }
            let Some(id) = app.open_note.borrow().clone() else {
                return;
            };
            app.notes.set_pinned(&id, btn.is_active());
            refresh_all(&app);
        });
    }
    {
        let app = app.clone();
        star_button.connect_toggled(move |btn| {
            if app.editor_suppress.get() {
                return;
            }
            let Some(id) = app.open_note.borrow().clone() else {
                return;
            };
            app.notes.set_favorite(&id, btn.is_active());
            refresh_list(&app);
        });
    }
    {
        let app = app.clone();
        delete_button.connect_clicked(move |_| {
            let Some(id) = app.open_note.borrow().clone() else {
                return;
            };
            if matches!(*app.view.borrow(), View::Bin) {
                crate::dialogs::confirm_purge(&app, &id);
            } else {
                crate::dialogs::confirm_delete(&app, &id);
            }
        });
    }
    {
        let app = app.clone();
        restore_button.connect_clicked(move |_| {
            let Some(id) = app.open_note.borrow().clone() else {
                return;
            };
            // A note edited in the bin keeps its last keystrokes when it is
            // recovered; commit while the buffer still holds it.
            commit_pending_edit(&app);
            app.notes.recover(&id);
            *app.open_note.borrow_mut() = None;
            refresh_all(&app);
        });
    }
    {
        let app = app.clone();
        purge_button.connect_clicked(move |_| {
            let Some(id) = app.open_note.borrow().clone() else {
                return;
            };
            crate::dialogs::confirm_purge(&app, &id);
        });
    }

    window.connect_close_request({
        let app = app.clone();
        move |_| {
            // The Swift port shipped without a shutdown flush and lost the
            // tail of every preference write on close. The editor's tail is
            // the same risk: a keystroke inside the debounce window would
            // never be committed once the main loop is gone.
            commit_pending_edit(&app);
            // The player writes its final bookmark from the session frontier
            // *before* the stores flush underneath it.
            crate::player::shutdown(&app);
            // The audiobook's final bookmark too: its tick is 250 ms, so the
            // last written position can be that stale, and the ffmpeg child
            // and the sink should go before the stores flush.
            crate::abook::shutdown(&app);
            app.flush_all();
            Propagation::Proceed
        }
    });

    {
        let app = app.clone();
        export_button.connect_clicked(move |_| export_open_note(&app));
    }
    {
        let app = app.clone();
        image_button.connect_clicked(move |_| insert_image_into_open_note(&app));
    }

    // Preview toggle. Turning preview on flushes any pending edit first, so
    // what is rendered is what is stored — otherwise a keystroke inside the
    // debounce window would be missing from the preview that replaces it.
    // The toggle also writes `renderMarkdown` — the same pref the settings
    // window's "Render markdown" switch writes — so the eye button and the
    // setting are one knob, and a note opens in reading mode whenever the
    // pref says so (iOS: "a note opens in reading mode; eye button to edit").
    {
        let app = app.clone();
        preview_toggle.connect_toggled(move |btn| {
            // `refresh_editor` syncs the button to the state it is already in.
            // Without this guard that round trip re-entered the handler, and
            // merely visiting the bin — where preview is hidden — cleared the
            // user's preference instead of just hiding the pane.
            if app.editor_suppress.get() {
                return;
            }
            let value = btn.is_active();
            app.preview.set(value);
            app.prefs.update(|p| p.render_markdown = value);
            commit_pending_edit(&app);
            // Force a re-render: the text may be the same but the pane was not
            // built, and an empty pane is worse than one extra parse.
            *app.preview_text.borrow_mut() = String::new();
            app.preview_note.borrow_mut().clear();
            refresh_editor(&app);
        });
    }

    // Expand-to-window. Both toolbar buttons and F11 funnel into
    // `apply_focus_mode`; its guard makes this handler idempotent when the
    // sync at the end of the function flips this button programmatically.
    {
        let app = app.clone();
        focus_toggle.connect_toggled(move |btn| {
            crate::window::apply_focus_mode(&app, btn.is_active());
        });
    }

    {
        let app = app.clone();
        sidebar_rail.connect_clicked(move |_| toggle_column(&app, 0));
    }
    {
        let app = app.clone();
        list_rail.connect_clicked(move |_| toggle_column(&app, 1));
    }
    {
        // The rails follow the dividers, not the clicks. A collapse is a
        // 220 ms slide, so the divider crosses the "gone" threshold long
        // after the click that started it — a rail that only re-synced on
        // its own click kept the "hide" glyph (and the "Hide the …" name)
        // while the column was already gone, and the column could not be
        // brought back by the same button. The position signal fires on
        // every step of the slide and on every drag.
        let app = app.clone();
        let rail = sidebar_rail.clone();
        root.connect_position_notify(move |_| {
            if let Some(w) = app.widgets.borrow().clone() {
                sync_rails(&w, Some(&rail));
            }
        });
    }
    {
        let app = app.clone();
        let rail = list_rail.clone();
        list_editor.connect_position_notify(move |_| {
            if let Some(w) = app.widgets.borrow().clone() {
                sync_rails(&w, Some(&rail));
            }
        });
    }
    {
        let app = app.clone();
        shelf_list_toggle.connect_toggled(move |btn| {
            if !btn.is_active() || app.prefs.get().book_shelf_is_list() {
                return;
            }
            app.prefs.update(|p| p.book_shelf_view = "list".to_string());
            refresh_list(&app);
        });
    }
    {
        let app = app.clone();
        shelf_cards_toggle.connect_toggled(move |btn| {
            if !btn.is_active() || !app.prefs.get().book_shelf_is_list() {
                return;
            }
            // Back to whatever card size the user last chose — the size
            // cell keeps it while list mode shows (the pref itself holds
            // "list" until this write).
            let mode = SHELF_MODES[app.shelf_mode.get().min(3) as usize].0;
            app.prefs.update(|p| p.book_shelf_view = mode.to_string());
            shelf_size_label(&app);
            refresh_list(&app);
        });
    }
    {
        let app = app.clone();
        let book_shelf = book_shelf.clone();
        shelf_columns.connect_selected_notify(move |drop| {
            // The rows are 1..=6, in order; the pref is the same number.
            let columns = drop.selected() + 1;
            app.prefs.update(|p| p.book_shelf_columns = columns);
            book_shelf.set_max_children_per_line(columns);
        });
    }

    install_shortcuts(&window, app);
    // Fixed 2026-11-02: the reader page holds the keyboard
    // focus, and this is what puts it back when the focus drops. Without a
    // focused widget GTK4 dispatches no key event at all, so the reader's
    // arrows died every time a hidden widget (the fill-the-window toolbar,
    // a replaced button) took the focus with it.
    install_focus_watchdog(&window, app);

    // The background channels. `tasks` carries the reader's workers (the
    // open, the page text, the search, the import) and `imgcache` the
    // picture decodes; both are attached before anything can post into
    // them, so a result is never lost.
    crate::tasks::attach(app);
    crate::imgcache::attach();
    // The library sweep runs alongside the first window: it walks every
    // manifest and rasterises a cover for any PDF that has none, which is
    // seconds of work that used to happen before the first frame.
    crate::reader::start_repair_sweep();

    // Spawn the reader's two web processes now rather than on the first
    // book or deck open (beta, round 4, loading pass — see reader::prewarm).
    crate::reader::prewarm(&reader);

    *app.widgets.borrow_mut() = Some(Widgets {
        window: window.clone(),
        sidebar_box: sidebar,
        notes_list,
        notes_scroll: list_scroll,
        book_shelf,
        shelf_scroll,
        search,
        sort_button,
        new_note_button,
        new_nb_button,
        import_book_button,
        settings_button,
        exit_focus,
        editor_title,
        editor_text,
        editor_status,
        editor_stack,
        editor_empty,
        preview_box,
        preview_toggle,
        focus_toggle,
        toolbar: toolbar_scroll,
        panes: (root.clone(), list_editor.clone()),
        slides: (
            crate::motion::Slide::new(&root),
            crate::motion::Slide::new(&list_editor),
        ),
        sidebar_rail: sidebar_rail.clone(),
        list_rail: list_rail.clone(),
        focus_anim: crate::motion::Anim::new(),
        format_bar: format_bar_scroll,
        pin_button,
        star_button,
        delete_button,
        export_button,
        image_button,
        bin_tools,
        list_header,
        shelf_controls,
        shelf_columns,
        shelf_size_button,
        player,
        player_scroll,
        picker,
        reader,
        pomodoro,
    });

    // `SN_FULLSCREEN=1` starts fullscreen: how the batch gates photograph a
    // real window on this box, where focus fights make partial shots miss.
    if std::env::var_os("SN_FULLSCREEN").is_some() {
        window.fullscreen();
    }
    window.present();
    // A book opened on the command line belongs to the *books* view, not to
    // the notes list the window starts on. Without this the reader's page is
    // behind a stack the view switch would replace, and the window shows a
    // list of notes with a book hidden behind it.
    //
    // The switch comes *before* the book opens: `open_book` fills the
    // reader's page and shows it, and a later `refresh_all` would rebuild
    // the list and leave the editor stack on the notes page.
    if crate::reader::has_pending_open() {
        *app.view.borrow_mut() = View::Books;
        *app.list_key.borrow_mut() = String::new();
    }
    crate::player::serve_autoplay(app);
    crate::reader::serve_pending_open(app);
    crate::abook::serve_pending_open(app);

    // Proving the window actually mapped matters: the family's history has a
    // launcher bug where the process ran and no window ever appeared. Three
    // independent signals, because each has lied before:
    //  - `connect_map` fires when the compositor maps the surface,
    //  - `gtk_widget_get_mapped` is what GTK believes,
    //  - the later `activate` for a second launch re-presents, which only works
    //    if a window object exists at all.
    {
        let w = window.clone();
        window.connect_map(move |_| {
            tracing::info!(
                mapped = w.is_mapped(),
                visible = w.is_visible(),
                realized = w.is_realized(),
                "window map signal"
            );
        });
    }
    tracing::info!(
        realized = window.is_realized(),
        mapped = window.is_mapped(),
        "window presented"
    );

    refresh_all(app);
    crate::dialogs::offer_resume(app);
}

/// The binary's entry into the GTK main loop.
///
/// The logging ring is installed before the window opens, so the first lines of
/// a launch land in the file; `apply_theme` runs at startup, before the first
/// window is realised, so the first frame is already themed. The icons are
/// mounted first for the same reason — an icon the theme resolved as missing
/// before the resource was registered keeps the placeholder for the life of the
/// widget that asked.
pub fn run_entry() -> i32 {
    let ring = sn_core::LogRing::new();
    sn_core::logging::init(ring);

    let app = match crate::app::App::open() {
        Ok(a) => a,
        Err(e) => {
            tracing::error!(error = %e, "cannot open the library");
            eprintln!("speechnotes: cannot open the library: {e}");
            return 1;
        }
    };

    let gio_app = gtk::Application::builder()
        .application_id(crate::app::APP_ID)
        .build();

    // Startup, before any window exists. Three things must happen before the first
    // frame is drawn, in this order:
    //
    //   1. Mount the shipped icons. Seven toolbar icon names exist only in
    //      Adwaita; on a Cosmic box they drew the "image-missing" placeholder.
    //      Registering the resource now means the first widget that asks for one
    //      gets the real thing — an icon the theme resolved as missing before it
    //      was registered keeps the placeholder for the life of the widget.
    //   2. Name the window icon: every window (and the shell's taskbar entry,
    //      when the desktop file is out of reach) falls back to the app icon,
    //      which the shipped theme resolves to the rounded-corner mark the iOS
    //      and Android launchers show. Here rather than before `run` because
    //      GTK must be initialised first — the static panicked at launch.
    //   3. Apply the theme, so the first frame is already themed.
    {
        let app = app.clone();
        gio_app.connect_startup(move |_| {
            crate::icons::register();
            gtk::Window::set_default_icon_name(crate::icons::APP_ICON);
            apply_theme(&app);
            // The page cache's old rasteriser wrote PNG pages; the new one
            // writes JPEG. The old files are dead weight — 477 MB on this
            // box, on a disk that was 97% full. The sweep is a few thousand
            // unlinks, so it goes to a worker rather than the startup path.
            crate::tasks::spawn_detached(crate::pdfview::sweep_stale_page_cache);
        });
    }

    // `activate` fires twice in normal operation: once for the primary launch
    // (no window yet) and once for every *later* launch, because GApplication
    // uniqueness makes a second process hand its activation to the primary and
    // exit. Building a window on that second signal produced two windows, each
    // with its own list, fighting over the same files — so the handler is
    // strictly conditional:
    //
    //   no window yet  → build one (first launch)
    //   window exists  → raise it and nothing else (every later launch)
    //
    // This is the launcher bug the Swift port shipped with, in the opposite
    // direction: it did nothing on the second activate, so a launcher tap
    // appeared to do nothing at all.
    {
        let app = app.clone();
        gio_app.connect_activate(move |gio_app| {
            let widgets = app.widgets.borrow().clone();
            match widgets {
                None => {
                    tracing::info!("first activate: building the window");
                    build_window(gio_app, &app);
                    // Nearby BookDrop does NOT auto-start with the window:
                    // `App::open` reset the persisted toggle, so the
                    // receiver comes up OFF on every launch and the user
                    // switches it on from Settings when they want to
                    // receive. The receiver's heartbeat below runs for the
                    // app's whole life, so a toggle-on is honoured the
                    // moment it happens.
                    crate::bookdrop::attach_app_poll(&app);
                }
                Some(w) => {
                    tracing::info!("later activate: raising the existing window");
                    w.window.present();
                }
            }
        });
    }

    // `run_with_args(&[])`, not `run()`: GApplication auto-parses the
    // process argv and would reject the binary's own flags (`--play-note`)
    // as unknown options. The binary consumes its flags before this point;
    // the application layer wants none of them.
    let code = gio_app.run_with_args::<&str>(&[]);
    app.flush_all();
    i32::from(code)
}

/// Show a failure once, in the app's own window. A silent failure here would
/// look to the user like the button did nothing.
pub fn report_failure(app: &Rc<App>, message: &str) {
    tracing::error!("{message}");
    let Some(w) = app.widgets.borrow().clone() else {
        eprintln!("speechnotes: {message}");
        return;
    };
    let dialog = gtk::AlertDialog::builder().message(message).modal(true).build();
    dialog.set_buttons(&["OK"]);
    dialog.choose(Some(&w.window), None::<&gtk::gio::Cancellable>, |_| {});
}
