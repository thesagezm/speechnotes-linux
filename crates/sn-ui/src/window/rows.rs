//! Row and shelf-card builders for the notes list and the books shelf
//! (split out of `window.rs`, Round 4, pure code-motion).

use std::rc::Rc;

use gtk4::prelude::*;
use gtk4 as gtk;

use sn_store::model::Note;

use crate::app::App;

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

/// Rows carry their note id in `App.row_ids`, keyed by row index. GObject
/// typed data would also work but is an `unsafe` accessor for no gain.
pub(crate) fn row_data(app: &Rc<App>, row: &gtk::ListBoxRow) -> Option<String> {
    let index = row.index();
    if index < 0 {
        return None;
    }
    // An empty slot is a header or the empty-state row. Handing that back as if
    // it were an id would make a click on a section label "open" a note whose id
    // is the empty string.
    app.row_ids
        .borrow()
        .get(index as usize)
        .filter(|id| !id.is_empty())
        .cloned()
}

pub(crate) fn section_row(label: &str) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::builder()
        .selectable(false)
        .activatable(false)
        .css_classes(["section-label"])
        .build();
    row.set_child(Some(&gtk::Label::builder()
        .label(label.to_uppercase())
        .halign(gtk::Align::Start)
        .build()));
    row
}

pub(crate) fn empty_row(text: &str) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::builder().selectable(false).build();
    let label = gtk::Label::builder().label(text).css_classes(["empty"]).build();
    row.set_child(Some(&label));
    row
}

pub(crate) fn note_row(note: &Note) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();

    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .build();

    let marker = gtk::Label::new(None);
    if note.is_pinned {
        marker.set_text("●");
        marker.add_css_class("row-meta");
    } else {
        marker.set_text(" ");
    }
    column.append(&marker);

    let text = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(1).build();
    let title = gtk::Label::builder()
        .label(if needs_placeholder_title(note) {
            "Untitled note".to_string()
        } else {
            note.title()
        })
        .halign(gtk::Align::Start)
        .single_line_mode(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["row-title"])
        .build();
    let preview_text = preview_of(note);
    let preview = gtk::Label::builder()
        .label(if preview_text.is_empty() {
            "No text yet".to_string()
        } else {
            preview_text
        })
        .halign(gtk::Align::Start)
        .single_line_mode(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["row-preview"])
        .build();
    let meta = gtk::Label::builder()
        .label(row_meta(note))
        .halign(gtk::Align::Start)
        .single_line_mode(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["row-meta"])
        .build();
    text.append(&title);
    text.append(&preview);
    text.append(&meta);
    text.set_hexpand(true);
    column.append(&text);

    if note.is_favorite {
        let star = gtk::Image::from_icon_name("starred-symbolic");
        column.append(&star);
    }

    row.set_child(Some(&column));
    row
}

/// A book on the shelf: its cover, its title, its author, and how far in
/// the reader is. Clicking it opens the book where the bookmark left it.
///
/// The card row height for a shelf of `shelf_height` in view mode `mode`:
/// a fraction of the shelf — ten rows for small icons, six for tiles, four
/// for large, two-and-a-half for very large — clamped to a floor and a
/// ceiling per mode. The floor is what makes the row count *decrease* on a
/// shorter window (the cards stop shrinking; fewer rows fit); the ceiling
/// keeps an unusually tall window from minting monsters.
///
/// The ceilings are the Oct 25 sizes: the cards grew with the window until
/// they were "humungous", because the old row height was a *sixth of the
/// shelf* with a 240 px cap that only the tall modes reached. A cover is a
/// shelf object, not a poster — it should not scale with the window, and
/// the user should see several books at once, not one.
pub(crate) fn shelf_row_height(shelf_height: i32, mode: &str) -> i32 {
    match mode {
        "small" => (shelf_height / 10).clamp(64, 116),
        "large" => (shelf_height / 4).clamp(150, 200),
        "vlarge" => (shelf_height * 2 / 5).clamp(220, 260),
        _ => (shelf_height / 6).clamp(96, 180),
    }
}

/// The cover's size inside a row of `row_height` px: the row minus the text
/// block under it (title+author+meta+progress in the big modes, title+bar
/// only in small), and the width from the cover's own ~0.72 book aspect.
///
/// The Oct 25 sizes were a 110×154 cover for the tiles mode. The cover is
/// capped at that size in every mode: a book cover is read, not admired,
/// and a taller window must not make the cards taller — it should make
/// *more* rows fit.
pub(crate) fn cover_size_for_row(row_height: i32, mode: &str) -> (i32, i32) {
    let (block, floor_h, cap_h) = match mode {
        "small" => (22, 40, 84),
        "large" => (40, 56, 154),
        "vlarge" => (40, 56, 200),
        _ => (40, 56, 154),
    };
    let cover_h = (row_height - block).min(cap_h);
    let cover_w = ((cover_h as f64) * 0.72).round() as i32;
    (cover_w.max(40), cover_h.max(floor_h))
}

/// The cover's **width** for a card: the cover is as wide as the card, and
/// its height follows the picture's own aspect.
///
/// The card's width is the cover's width plus the CSS padding, so the two
/// are one number. Deriving the cover's width from the *row* height (as
/// `cover_size_for_row` does) assumed a book-shaped cover; a cover file
/// that is wider than 0.72 (a scan, a boxed set) came out narrower than
/// the card and left a band of empty card beside it.
pub(crate) fn cover_width_for_card(mode: &str) -> i32 {
    match mode {
        "small" => 64,
        _ => 110,
    }
}

/// The card's css class for `mode` — theme.css scales the text per mode.
pub(crate) fn shelf_mode_class(mode: &str) -> &'static str {
    match mode {
        "small" => "shelf-small",
        "large" => "shelf-large",
        "vlarge" => "shelf-vlarge",
        _ => "shelf-tiles",
    }
}

/// Apply the row's card size to every card on the shelf. The empty-state
/// card holds a label, not a cover box, and skips by the downcast.
pub(crate) fn resize_book_cards(shelf: &gtk::FlowBox, row_height: i32, mode: &str) {
    // The cover's width is the card's width; its height comes from the
    // row. The picture inside keeps its own aspect, so a cover that is
    // not book-shaped is letterboxed rather than stretched.
    let cover_w = cover_width_for_card(mode);
    let (_, cover_h) = cover_size_for_row(row_height, mode);
    let mut child = shelf.first_child();
    while let Some(cell) = child {
        child = cell.next_sibling();
        let Some(flow_child) = cell.downcast_ref::<gtk::FlowBoxChild>() else {
            continue;
        };
        let Some(card) = flow_child.child().and_downcast::<gtk::Box>() else {
            continue;
        };
        // The card's minimum width is the cover plus the CSS padding
        // (10 px each side) and the border. The card itself fills its
        // uniform FlowBox cell (see `book_card`), so every card on the
        // shelf wears the same width whatever its title; this floor only
        // keeps a card from measuring narrower than its cover.
        card.set_width_request(cover_w + 22);
        // The cover is the card's first child. It is the cached picture's
        // *wrapper* box, and the picture inside it carries its own size
        // request — so both have to be resized, or the inner one wins and
        // the card keeps the picture's natural height.
        if let Some(cover) = card.first_child().and_downcast::<gtk::Box>() {
            cover.set_width_request(cover_w);
            cover.set_height_request(cover_h);
            if let Some(inner) = cover.first_child() {
                inner.set_width_request(cover_w);
                inner.set_height_request(cover_h);
                // The picture inside keeps its own aspect, so it must be
                // told the new box to fit inside — the cap is what stops
                // it reporting its natural size and stretching the card.
                if let Some(picture) = inner
                    .first_child()
                    .and_downcast::<crate::aspect_image::AspectImage>()
                {
                    picture.set_cap(cover_w, cover_h);
                }
            }
        }
    }
}

/// The card-size modes the shelf's cycle button walks through, in order:
/// (pref value, label). The label doubles as the button's accessible name,
/// so it always announces the mode on screen.
pub(crate) const SHELF_MODES: [(&str, &str); 4] = [
    ("small", "Small"),
    ("tiles", "Tiles"),
    ("large", "Large"),
    ("vlarge", "Huge"),
];

/// The size-cycle button's label announces the current card mode — its
/// accessible name IS the state, which is what keeps it drivable.
pub(crate) fn shelf_size_label(app: &Rc<App>) {
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    let mode = app.prefs.get().book_shelf_mode();
    let label = SHELF_MODES
        .iter()
        .find(|(value, _)| *value == mode)
        .map(|(_, l)| *l)
        .unwrap_or("Tiles");
    w.shelf_size_button.set_label(label);
}

/// The book list's row sizes, indexed by the `book_list_size` pref:
/// (thumb width, thumb height, the row's CSS class). Text size rides the
/// class (`.booklist-*` rules in theme.css) so the four steps stay four —
/// a continuous factor would need per-row text attributes everywhere a row
/// is built. The default step is the Oct-23 look the user knows.
pub(crate) const BOOK_LIST_SIZES: [(i32, i32, &str); 4] = [
    (30, 42, "booklist-compact"),
    (38, 54, "booklist-default"),
    (46, 66, "booklist-comfy"),
    (56, 80, "booklist-large"),
];

pub(crate) fn book_card(app: &Rc<App>, book: &sn_store::books::Book, mode: &str) -> gtk::Box {
    let small = mode == "small";
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["book-card", shelf_mode_class(mode)])
        // The card **fills its cell**, so every card on the shelf wears the
        // same width. The cell is the FlowBox's uniform item size — the
        // largest natural card width across the shelf (see the shelf's
        // builder in `window.rs`, whose halign/valign stop the cells
        // stretching to the pane). A card that centred itself in that cell
        // instead rendered at its own natural width, which is the title's
        // to set: a short title gave a narrow card beside a wide one, and
        // the shelf read as ragged. Uniform width is the shelf's one rule;
        // the cover and the labels sit inside the card, so a wider cell
        // reads as margin, not as a different card.
        //
        // The width request is only the floor the card wears before the
        // first measure; the cover's real size is set by the shelf's tick
        // callback (`resize_book_cards`, a mode-chosen fraction of the
        // shelf's height per row). A narrow pane reflows below the column
        // ceiling — the cards themselves never stretch.
        .width_request(if small { 64 } else { 110 })
        .halign(gtk::Align::Fill)
        .build();

    let cover: gtk::Widget = match book.cover() {
        Some(path) if path.exists() => {
            // Through the cache: decode off the UI thread, downscaled to the
            // cover's own cap, and sniffed from the bytes rather than the
            // name (a cover on this box is a PNG wearing a .jpg). The
            // AspectImage inside keeps the cover's own aspect — no crop, no
            // distortion, whatever the card's width.
            //
            // The *sized* variant, not the full-width one: a page image
            // fills whatever width it is given, and its height follows from
            // its own aspect, so the cover grew to the image's size and
            // overrode the card's height request entirely.
            let (cover_w, cover_h) = cover_size_for_row(110, mode);
            let picture = crate::imgcache::sized_picture(
                &path,
                crate::imgcache::COVER_MAX_EDGE,
                cover_w,
                cover_h,
            );
            picture.upcast()
        }
        _ => {
            let placeholder = gtk::Box::builder()
                .width_request(if small { 64 } else { 110 })
                .height_request(if small { 84 } else { 154 })
                .halign(gtk::Align::Center)
                .valign(gtk::Align::Center)
                .css_classes(["book-cover-placeholder"])
                .build();
            placeholder.append(&gtk4::Image::from_icon_name("audiobook-symbolic"));
            placeholder.upcast()
        }
    };
    card.append(&cover);

    let title = gtk4::Label::builder()
        .label(&book.manifest.title)
        .halign(gtk::Align::Start)
        .wrap(!small)
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        // The title's *natural* width is the whole title's width even when
        // it wraps, so a long book name set the card's minimum width and
        // the card grew to the FlowBox cell's width — the shelf became one
        // column of wide cards. The cap is in characters, so the label
        // asks for no more than the cover is wide.
        .max_width_chars(if small { 10 } else { 14 })
        .width_request(if small { 64 } else { 110 })
        .css_classes(["book-title"])
        .build();
    card.append(&title);

    if !small {
        let author = gtk4::Label::builder()
            .label(if book.manifest.author.is_empty() {
                "Unknown author".to_string()
            } else {
                book.manifest.author.clone()
            })
            .halign(gtk::Align::Start)
            .single_line_mode(true)
            .ellipsize(gtk4::pango::EllipsizeMode::End)
            .max_width_chars(16)
            .width_request(110)
            .css_classes(["book-author"])
            .build();
        card.append(&author);
    }

    // The meta line: duration for a listener, chapters for a reader.
    let total = if book.is_audiobook() {
        book.manifest.chapters.len()
    } else {
        book.manifest.spine_count
    };
    let position = book.manifest.position.clone();
    let fraction = if total == 0 {
        0.0
    } else {
        ((position.chapter_index as f64 + position.chapter_fraction.clamp(0.0, 1.0))
            / total as f64)
            .clamp(0.0, 1.0)
    };
    if !small {
        let percent = (fraction * 100.0).round() as usize;
        let meta_label = if book.is_audiobook() {
            let seconds = book.manifest.duration_seconds.unwrap_or(0.0);
            let hours = seconds / 3600.0;
            if hours >= 1.0 {
                format!("{hours:.1} hours · {percent}%")
            } else {
                format!("{:.0} min · {percent}%", seconds / 60.0)
            }
        } else {
            format!("{} chapters · {percent}%", book.manifest.spine_count)
        };
        let meta = gtk4::Label::builder()
            .label(meta_label)
            .halign(gtk::Align::Start)
            .single_line_mode(true)
            .ellipsize(gtk4::pango::EllipsizeMode::End)
            .max_width_chars(18)
            .width_request(110)
            .css_classes(["book-meta"])
            .build();
        card.append(&meta);
    }

    let progress = gtk4::ProgressBar::builder()
        .fraction(fraction)
        .css_classes(["book-progress"])
        .build();
    card.append(&progress);

    // One click opens the book at its bookmark. The menu's clone is taken
    // first: the click closure's `app` is a move, not a borrow.
    let menu_app = app.clone();
    let click = gtk4::GestureClick::new();
    let app = app.clone();
    let id = book.manifest.id.clone();
    let chapter = book.manifest.position.chapter_index;
    click.connect_pressed(move |_, _, _, _| {
        crate::reader::open_book(&app, &id, chapter);
    });
    card.add_controller(click);

    // Right-click offers the delete. Books had no delete path at all — the
    // store had `BooksStore::delete` and no caller — and the shelf is where
    // a book is chosen, so it is where one is retired too. The popover is
    // built at click time and parented to the card then; a popover held
    // ready in every card would be one hidden widget per book on the shelf.
    let menu = gtk::GestureClick::new();
    menu.set_button(3);
    {
        let app = menu_app;
        let book = book.clone();
        // A weak handle: the gesture is owned by the card, so a strong card
        // clone inside the handler would hold the card alive after its
        // parent (the shelf rebuild) has dropped it.
        let weak_card = card.downgrade();
        menu.connect_released(move |_, _, x, y| {
            let Some(card) = weak_card.upgrade() else {
                return;
            };
            let popover = book_card_menu(&app, &book);
            popover.set_parent(&card);
            popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
                x as i32,
                y as i32,
                1,
                1,
            )));
            popover.popup();
        });
    }
    card.add_controller(menu);

    card
}

/// The right-click menu of a book on the shelf: "Delete book…", the action
/// the card's gesture offers. Built per click, parented by the caller.
fn book_card_menu(app: &Rc<App>, book: &sn_store::books::Book) -> gtk::Popover {
    let popover = gtk::Popover::new();
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(0)
        .margin_top(4)
        .margin_bottom(4)
        .margin_start(4)
        .margin_end(4)
        .build();
    let delete = gtk::Button::builder()
        .label("Delete book…")
        .css_classes(["flat"])
        .build();
    let weak_popover = popover.downgrade();
    {
        let app = app.clone();
        let book = book.clone();
        delete.connect_clicked(move |_| {
            if let Some(popover) = weak_popover.upgrade() {
                popover.popdown();
            }
            crate::dialogs::confirm_delete_book(&app, book.clone());
        });
    }
    column.append(&delete);
    popover.set_child(Some(&column));
    popover
}

/// The books view's list mode: the row the shelf replaced. One row per book
/// — glyph, title, author and how far in — and the list's own selection
/// handler opens it, because in this view the row id IS a book id. The
/// labels carry width caps: an ellipsized label still reports its whole text
/// as its natural width, so without the caps a long title would set the
/// pane's minimum width.
pub(crate) fn book_row(app: &Rc<App>, book: &sn_store::books::Book, size: usize) -> gtk::ListBoxRow {
    let (thumb_w, thumb_h, size_class) = BOOK_LIST_SIZES[size.min(BOOK_LIST_SIZES.len() - 1)];
    let row = gtk::ListBoxRow::new();
    row.set_css_classes(&["book-row", size_class]);

    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .margin_top(5)
        .margin_bottom(5)
        .margin_start(6)
        .margin_end(6)
        .build();

    // The cover thumbnail — the one thing a book row has that a note row
    // does not. Decoded through the same sniffed, cached pipeline the cards
    // use, at the row's own size; the glyph stays only for a book with no
    // cover at all.
    //
    // The thumb is a *thumbnail*: the Oct 23 row had no cover at all, and
    // the cover that arrived with the shelf's appearance layer was sized
    // for a card, not for a 54 px row. `sized_picture` is the variant that
    // caps the picture — the full-width `picture` reports the texture's own
    // size as its minimum, and a `set_size_request` afterwards is only a
    // floor the texture overrode, which is how these rows grew to ~500 px.
    let thumb: gtk::Widget = match book.cover() {
        Some(path) if path.exists() => {
            crate::imgcache::sized_picture(&path, (thumb_w * 3).max(96) as u32, thumb_w, thumb_h)
                .upcast()
        }
        _ => {
            let ph = gtk::Box::builder()
                .width_request(thumb_w)
                .height_request(thumb_h)
                .css_classes(["book-cover-placeholder", "book-row-thumb"])
                .build();
            ph.append(&gtk4::Image::from_icon_name(if book.is_audiobook() {
                "audiobook-symbolic"
            } else {
                "view-contents-symbolic"
            }));
            ph.upcast()
        }
    };
    column.append(&thumb);

    // The text box expands, so the row's text spans whatever the pane gives
    // the row and the ellipsis lands at the row's real edge. The old
    // `max_width_chars` caps (18 title / 28 meta) were tuned for the Oct-23
    // rows and left the right half of every row dead: live extents measured
    // titles cut at ~200 px inside a 388 px row — the user's "the list stops
    // part-way". No cap is safe here the way it is not in a plain Box: the
    // list wears a ScrolledWindow that does not propagate natural width, so
    // the pane's minimum is the rows' *minimum* (an ellipsizing label's is
    // one word wide, not its whole text) — the same shape the notes rows
    // have always had.
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(1)
        .valign(gtk::Align::Center)
        .hexpand(true)
        .build();
    text.append(
        &gtk::Label::builder()
            .label(&book.manifest.title)
            .halign(gtk::Align::Start)
            .single_line_mode(true)
            .ellipsize(gtk4::pango::EllipsizeMode::End)
            .css_classes(["row-title"])
            .build(),
    );

    let author = if book.manifest.author.is_empty() {
        "Unknown author".to_string()
    } else {
        book.manifest.author.clone()
    };
    let total = if book.is_audiobook() {
        book.manifest.chapters.len()
    } else {
        book.manifest.spine_count
    };
    let position = &book.manifest.position;
    let progress = if total == 0 {
        String::new()
    } else {
        let fraction = ((position.chapter_index as f64
            + position.chapter_fraction.clamp(0.0, 1.0))
            / total as f64)
            .clamp(0.0, 1.0);
        format!(" · {}% in", (fraction * 100.0).round() as usize)
    };
    let meta_label = if book.is_audiobook() {
        let seconds = book.manifest.duration_seconds.unwrap_or(0.0);
        let hours = seconds / 3600.0;
        let length = if hours >= 1.0 {
            format!("{hours:.1} hours")
        } else {
            format!("{:.0} min", seconds / 60.0)
        };
        format!("{author} · {length}{progress}")
    } else {
        format!("{author} · {total} chapters{progress}")
    };
    text.append(
        &gtk::Label::builder()
            .label(meta_label)
            .halign(gtk::Align::Start)
            .single_line_mode(true)
            .ellipsize(gtk4::pango::EllipsizeMode::End)
            .css_classes(["row-preview"])
            .build(),
    );
    column.append(&text);
    row.set_child(Some(&column));

    // The list mode's right-click is the card's: one menu, one delete, and
    // no dependence on the row being selected first.
    let menu = gtk::GestureClick::new();
    menu.set_button(3);
    {
        let app = app.clone();
        let book = book.clone();
        let weak_row = row.downgrade();
        menu.connect_released(move |_, _, x, y| {
            let Some(row) = weak_row.upgrade() else {
                return;
            };
            let popover = book_card_menu(&app, &book);
            popover.set_parent(&row);
            popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
                x as i32,
                y as i32,
                1,
                1,
            )));
            popover.popup();
        });
    }
    row.add_controller(menu);
    row
}

pub(crate) fn bin_row(note: &Note) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    let label = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .single_line_mode(true)
        .css_classes(["row-title"])
        .label(note.title())
        .build();
    let when = note
        .deleted_at
        .map(|d| format!("deleted {}", d.local_day()))
        .unwrap_or_else(|| "deleted".into());
    let meta = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .css_classes(["row-meta"])
        .label(when)
        .build();
    let column = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(1).build();
    column.append(&label);
    column.append(&meta);
    row.set_child(Some(&column));
    row
}

/// First line of the body, capped, for the list preview.
pub(crate) fn preview_of(note: &Note) -> String {
    let plain: String = note.text.lines().map(|l| l.trim()).find(|l| !l.is_empty()).unwrap_or("").to_string();
    let mut out: String = plain.chars().take(90).collect();
    if plain.chars().count() > 90 {
        out.push('…');
    }
    out
}

pub(crate) fn row_meta(note: &Note) -> String {
    let words = note.word_count();
    format!(
        "{} · {} words · ~{} min",
        note.updated_at.local_day(),
        words,
        note.listen_minutes()
    )
}

/// True when the note has no title, no body, and nothing else worth showing.
///
/// An empty note gets an explicit "Untitled note" title rather than three
/// blank lines, so a list of several empty notes is still readable.
pub(crate) fn needs_placeholder_title(note: &Note) -> bool {
    note.explicit_title.is_none()
        && note.text.trim().is_empty()
        && note.derived_title().is_none()
}

pub(crate) fn row_for(app: &Rc<App>, id: &str) -> Option<gtk::ListBoxRow> {
    let w = app.widgets.borrow().clone()?;
    let ids = app.row_ids.borrow();
    for (index, row_id) in ids.iter().enumerate() {
        if row_id == id {
            return w.notes_list.row_at_index(index as i32);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A rail's expand target is the width the user last chose — or the
    /// launch default when that record is no good. The two bad records are
    /// the ones that would otherwise strand a column: a divider remembered
    /// at zero by focus mode, and a width at or under the collapse threshold
    /// (a column crushed by a drag rather than collapsed by its rail).
    /// Expanding either back into a sliver would leave the rail no way out
    /// at all — the exact complaint the rails exist to answer.
    /// The shelf's card maths: one sixth of the shelf per row, floored and
    /// capped, and the cover carved out of the row on the book aspect.
    #[test]
    fn shelf_card_maths() {
        // A ~640 px shelf, tiles: six rows of ~106 px.
        assert_eq!(shelf_row_height(640, "tiles"), 106);
        // The clamp: a tall window stops at 180, a short one at 96.
        assert_eq!(shelf_row_height(2400, "tiles"), 180);
        assert_eq!(shelf_row_height(300, "tiles"), 96);
        // The other modes run their own fractions on the same shelf.
        assert_eq!(shelf_row_height(640, "small"), 64);
        assert_eq!(shelf_row_height(640, "large"), 160);
        assert_eq!(shelf_row_height(640, "vlarge"), 256);
        // Floors hold on a short window: the cards stop shrinking, the
        // shelf fits fewer rows instead.
        assert_eq!(shelf_row_height(300, "small"), 64);
        assert_eq!(shelf_row_height(300, "large"), 150);
        // Ceilings hold on a tall one. The cards must not grow without
        // limit: the user asked for the Oct 25 sizes back because the
        // covers had become "humungous".
        assert_eq!(shelf_row_height(2400, "vlarge"), 260);
        assert_eq!(shelf_row_height(2400, "large"), 200);
        // The cover is the row minus the text block, on the book aspect,
        // and never below a sensible minimum. Small icons carry a smaller
        // text block (title + bar only).
        let (w, h) = cover_size_for_row(106, "tiles");
        assert_eq!(h, 66);
        assert_eq!(w, 48);
        let (w, h) = cover_size_for_row(50, "tiles");
        assert_eq!((w, h), (40, 56));
        let (w, h) = cover_size_for_row(64, "small");
        assert_eq!((w, h), (40, 42));
        // The cover never grows past its cap, however tall the row is.
        let (w, h) = cover_size_for_row(1000, "vlarge");
        assert_eq!(h, 200);
        assert_eq!(w, 144);
        let (w, h) = cover_size_for_row(1000, "tiles");
        assert_eq!(h, 154);
        assert_eq!(w, 111);
    }
}
