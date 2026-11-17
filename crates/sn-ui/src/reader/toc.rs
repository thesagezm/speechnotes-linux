//! Split out of reader.rs 2026-10-31 (pure code-motion).

use std::rc::Rc;
use gtk4::prelude::*;

use crate::app::App;
use super::*;

// --- The contents sidebar ---------------------------------------------------
//
// The TOC was a `Popover` off the toolbar's Contents button. A popover
// renders on its own X surface: its rows sat beyond the reach of the
// a11y tree's click paths, it floated over the book instead of standing
// beside it, and a nested contents could only be *indented*, not folded.
// The sidebar is a column of the reader page itself — the button slides it
// in and out, its rows live in the main window's surface (so the driver and
// assistive tech reach them), and a nested contents renders as disclosure
// tiles: chapters within chapters.

/// One row of the book's contents, flat, as the manifest carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TocEntry {
    /// The chapter to open. `usize::MAX` for a row that points nowhere —
    /// an audiobook's placeholder — so activating it changes nothing
    /// instead of sailing to chapter 0.
    pub chapter: usize,
    pub label: String,
    pub depth: usize,
    /// The engine's own target, when the row came from the book view's
    /// report (a foliate href — `<section>#<anchor>` or the book's own
    /// path). A present href navigates the engine, which lands on the
    /// exact anchor the book's contents pointed at, not the section's top.
    pub href: Option<String>,
}

/// The contents as a tree: an entry and everything nested beneath it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TocNode {
    pub entry: TocEntry,
    pub children: Vec<TocNode>,
}

/// Nest the flat rows: a row at depth `d` is a child of the nearest previous
/// row with depth < `d` (a jump — 0 straight to 2 — still nests under that
/// nearest row, and a first row deeper than 0 roots at the top). The open
/// path is a stack whose depths strictly increase — pushing `d` pops
/// everything ≥ `d` — so the entries left on it at the end are one chain,
/// and the drain folds that chain up in order.
pub(crate) fn build_toc_tree(rows: &[TocEntry]) -> Vec<TocNode> {
    let mut root: Vec<TocNode> = Vec::new();
    let mut stack: Vec<TocNode> = Vec::new();
    for entry in rows {
        while stack.last().is_some_and(|n| n.entry.depth >= entry.depth) {
            let node = stack.pop().expect("checked non-empty");
            match stack.last_mut() {
                Some(parent) => parent.children.push(node),
                None => root.push(node),
            }
        }
        stack.push(TocNode {
            entry: entry.clone(),
            children: Vec::new(),
        });
    }
    while let Some(node) = stack.pop() {
        match stack.last_mut() {
            Some(parent) => parent.children.push(node),
            None => root.push(node),
        }
    }
    root
}

/// The chain of child indices from the tree's root down to the row whose
/// chapter is `page` — the leaf when one points there, otherwise the first
/// group row that names it (parents often carry the chapter their first
/// child opens, and the mark then lands on the tile header) — or `None`
/// when nothing does.
pub(crate) fn active_path(nodes: &[TocNode], page: usize) -> Option<Vec<usize>> {
    if page == usize::MAX {
        return None;
    }
    for (i, node) in nodes.iter().enumerate() {
        if node.children.is_empty() && node.entry.chapter == page {
            return Some(vec![i]);
        }
        if let Some(mut path) = active_path(&node.children, page) {
            path.insert(0, i);
            return Some(path);
        }
    }
    for (i, node) in nodes.iter().enumerate() {
        if node.entry.chapter == page {
            return Some(vec![i]);
        }
    }
    None
}

/// The sidebar's state: the book the rows belong to, every row that can
/// carry the active mark, and the groups. Lives on the **App**
/// (`app.toc_sidebar`), not in `Widgets` — `Widgets` derives `Clone`, and
/// a cloned `RefCell` is a deep copy whose writes are thrown away with the
/// clone. The widget handles (the panel's revealer, the rows' container)
/// live in `ReaderWidgets` as plain fields; cloning those shares the
/// widgets, which is all the state the layout needs.
#[derive(Clone, Default)]
pub struct TocSidebar {
    /// The book the rows belong to ("" = none yet).
    pub book_id: String,
    /// Every row that can carry the active mark: (chapter, button). Leaves
    /// and group headers both live here — a stale mark must clear from
    /// either.
    pub rows: Vec<(usize, gtk4::Button)>,
    /// The groups: (children revealer, arrow image, descendant chapters).
    /// Marking a row opens the groups that contain it.
    pub groups: Vec<(gtk4::Revealer, gtk4::Image, Vec<usize>)>,
}

/// Build the sidebar's shell: the panel box, its scroller, and the revealer
/// that slides it — the widget half, stored as plain fields in
/// `ReaderWidgets`. The rows arrive when a book opens — [`build_toc`] for a
/// book, [`fill_chapter_rows`] for an audiobook's chapters.
pub(crate) fn build_shell() -> (gtk4::Revealer, gtk4::Box, gtk4::Box) {
    let list = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(0)
        .build();
    let scroll = gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vscrollbar_policy(gtk4::PolicyType::Automatic)
        .vexpand(true)
        .child(&list)
        .build();
    let panel = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .css_classes(["toc-sidebar"])
        .width_request(300)
        .build();
    panel.append(&scroll);
    let revealer = gtk4::Revealer::builder()
        .transition_type(gtk4::RevealerTransitionType::SlideRight)
        .transition_duration(crate::motion::FADE_MS)
        .child(&panel)
        .build();
    (revealer, panel, list)
}

/// Append `nodes` to `into` as the sidebar's rows, collecting the widgets
/// [`mark_toc_row`] needs. `path` is the active chain ([`active_path`]) as
/// child indices: a tile opens when the path names it and continues
/// beneath it. Returns the chapters named by everything appended — a group
/// reports its whole subtree, so a deep leaf's ancestors all know to open
/// for a later mark.
#[allow(clippy::too_many_arguments)]
fn append_rows(
    into: &gtk4::Box,
    nodes: &[TocNode],
    rows: &mut Vec<(usize, gtk4::Button)>,
    groups: &mut Vec<(gtk4::Revealer, gtk4::Image, Vec<usize>)>,
    app: &Rc<App>,
    book_id: &str,
    path: Option<&[usize]>,
) -> Vec<usize> {
    let mut chapters = Vec::new();
    for (i, node) in nodes.iter().enumerate() {
        let entry = &node.entry;
        let indent = entry.depth.min(6) as i32;
        // The active path descends by child index: the slice continues
        // under the one node the path names, nowhere else.
        let child_path = path
            .filter(|p| p.first() == Some(&i))
            .and_then(|p| p.get(1..));
        if node.children.is_empty() {
            // A leaf: the row that opens a chapter. A row that points
            // nowhere renders and does nothing — the same rule the
            // popover's placeholder rows followed.
            let button = gtk4::Button::builder().css_classes(["flat", "toc-row"]).build();
            let label = gtk4::Label::builder()
                .label(&entry.label)
                .halign(gtk4::Align::Start)
                .wrap(true)
                .xalign(0.0)
                .max_width_chars(48 - indent)
                .margin_start(16 * indent)
                .build();
            button.set_child(Some(&label));
            if entry.chapter != usize::MAX && !book_id.is_empty() {
                let app = app.clone();
                let book_id = book_id.to_string();
                let chapter = entry.chapter;
                // An engine row navigates the engine — the href lands on
                // the exact anchor the book's contents pointed at (a
                // subsection, a footnote, a volume head), not merely the
                // section's top. Everything else opens the chapter the old
                // way.
                let href = entry.href.clone();
                button.connect_clicked(move |_| {
                    if let Some(href) = href.as_deref() {
                        if crate::reader::engine_row_navigates(&app) {
                            if let Some(w) = app.widgets.borrow().clone() {
                                w.reader.book.go_to(href);
                            }
                            return;
                        }
                    }
                    open_book(&app, &book_id, chapter);
                });
            }
            rows.push((entry.chapter, button.clone()));
            into.append(&button);
            chapters.push(entry.chapter);
        } else {
            // A group: a disclosure tile. The whole header toggles the
            // children; the arrow points down while they show and leans
            // right while they are folded away. A tile does not navigate —
            // its children do.
            let header = gtk4::Button::builder().css_classes(["flat", "toc-group"]).build();
            let arrow = gtk4::Image::from_icon_name("pan-down-symbolic");
            let label = gtk4::Label::builder()
                .label(&entry.label)
                .halign(gtk4::Align::Start)
                .wrap(true)
                .xalign(0.0)
                .max_width_chars(44 - indent)
                .build();
            let head_row = gtk4::Box::builder()
                .orientation(gtk4::Orientation::Horizontal)
                .spacing(6)
                .margin_start(16 * indent)
                .build();
            head_row.append(&arrow);
            head_row.append(&label);
            header.set_child(Some(&head_row));
            let children_box = gtk4::Box::builder()
                .orientation(gtk4::Orientation::Vertical)
                .spacing(0)
                .build();
            let descendants = append_rows(
                &children_box,
                &node.children,
                rows,
                groups,
                app,
                book_id,
                child_path,
            );
            let child_revealer = gtk4::Revealer::builder()
                .transition_type(gtk4::RevealerTransitionType::SlideDown)
                .transition_duration(crate::motion::FADE_MS)
                .child(&children_box)
                .build();
            // The tile opens when the active path runs through it — the
            // path names this tile and continues beneath it. The rest start
            // folded, so a textbook's contents reads as a handful of parts
            // instead of a wall of every row at once.
            let open_now = child_path.is_some_and(|p| !p.is_empty());
            child_revealer.set_reveal_child(open_now);
            if !open_now {
                arrow.add_css_class("toc-collapsed");
            }
            {
                let revealer = child_revealer.clone();
                let arrow = arrow.clone();
                header.connect_clicked(move |_| {
                    let reveal = !revealer.reveals_child();
                    revealer.set_reveal_child(reveal);
                    if reveal {
                        arrow.remove_css_class("toc-collapsed");
                    } else {
                        arrow.add_css_class("toc-collapsed");
                    }
                });
            }
            rows.push((entry.chapter, header.clone()));
            chapters.push(entry.chapter);
            chapters.extend(descendants.iter().copied());
            groups.push((child_revealer.clone(), arrow.clone(), descendants));
            into.append(&header);
            into.append(&child_revealer);
        }
    }
    chapters
}

/// Mark the row of the page now open, and slide open the groups above it.
pub(crate) fn mark_toc_row(app: &Rc<App>, page: usize) {
    let sidebar = app.toc_sidebar.borrow().clone();
    // Every marked row loses the mark first — the row that held it last
    // time is not the row the reader stands on now.
    for (_, button) in &sidebar.rows {
        button.remove_css_class("active");
    }
    let Some((_, button)) = sidebar
        .rows
        .iter()
        .find(|(chapter, _)| *chapter == page && *chapter != usize::MAX)
    else {
        return;
    };
    button.add_css_class("active");
    // The groups above it open with it: a mark folded inside a collapsed
    // tile is a mark the reader cannot see.
    if page != usize::MAX {
        for (revealer, arrow, descendants) in &sidebar.groups {
            if descendants.contains(&page) {
                revealer.set_reveal_child(true);
                arrow.remove_css_class("toc-collapsed");
            }
        }
    }
}

/// Pages ⇄ text for a PDF, book ⇄ read-along for an EPUB. One button,
/// because the reader shows one thing or the other, and the tooltip says
/// what pressing it will do.
pub(crate) fn toggle_pdf_mode(app: &Rc<App>) {
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    let format = app.reader.borrow().format.clone();
    let text_mode = {
        let mut state = app.reader.borrow_mut();
        let next = !state.pdf_text_mode;
        state.pdf_text_mode = next;
        next
    };
    // The EPUB's two faces: the book itself, or the parsed text the voice
    // reads. The word on the text side is the user's own — "Read along" —
    // because that view exists for following the voice, not for reading.
    let (book_page, text_page, book_icon, text_icon, book_tip, text_tip) = if format == "pdf" {
        (
            "pages",
            "pdf-text",
            //  exchanged with the TOC button's icon (user report).
            "view-contents-symbolic",
            "view-paged-symbolic",
            "Read the extracted text instead of the pages",
            "Show the rendered pages",
        )
    } else if is_slides_format(&format) {
        // The deck read-along dark-page bug: an open deck
        // fell through to the EPUB pages above, so the toggle showed the
        // EPUB's empty read-along view — a dark canvas with no words — and
        // the deck's own pages were unreachable until the book was
        // reopened. The deck gets its own pair, the pages `open_slides`
        // set up.
        (
            "slides",
            "slides-text",
            "view-contents-symbolic",
            "view-paged-symbolic",
            "Show the slides",
            "Read along with the slide text",
        )
    } else {
        // An engine document's book page is the engine's, an EPUB's is the
        // per-spine view's — the read-along text is the same pane either way.
        let engine = app.reader.borrow().engine_rendered;
        (
            if engine { "engine" } else { "book" },
            "text",
            //  exchanged with the TOC button's icon (user report).
            "view-contents-symbolic",
            "view-paged-symbolic",
            "Read along with the parsed text",
            "Show the book as the publisher made it",
        )
    };
    if text_mode {
        w.reader.stack.set_visible_child_name(text_page);
        w.reader.mode_toggle.set_icon_name(text_icon);
        w.reader.mode_toggle.set_tooltip_text(Some(book_tip));
    } else {
        w.reader.stack.set_visible_child_name(book_page);
        w.reader.mode_toggle.set_icon_name(book_icon);
        w.reader.mode_toggle.set_tooltip_text(Some(text_tip));
    }
}

/// The TOC row's label for a spine index.
pub(crate) fn toc_label_for(book: &sn_store::books::Book, index: usize) -> Option<String> {
    book.manifest
        .toc
        .iter()
        .find(|row| row.spine_index == Some(index))
        .map(|row| row.label.clone())
}

/// Fill the contents sidebar from the manifest.
///
/// The rows come from the manifest's `toc`, nested by depth: a row with
/// children renders as a disclosure tile, its deeper rows folded inside it
/// (the tile holding the open chapter starts open, the rest folded). The
/// row for the open chapter carries the `active` class, which the sidebar's
/// CSS paints; without it the reader has no idea where in its own contents
/// it stands. The panel scrolls — a 73-chapter textbook's contents is 71
/// rows, and the sidebar is a column of the page, not an unbounded surface.
pub(crate) fn build_toc(app: &Rc<App>, book: &sn_store::books::Book) {
    let current = app.reader.borrow().chapter;
    let mut entries: Vec<TocEntry> = Vec::new();
    for row in &book.manifest.toc {
        let label = if book.manifest.format == "pdf" {
            // The outline's own label when it is a real one; LibreOffice's
            // "Paediatric cardiology (3)" rows are the outline repeating the
            // title, which the contents already says. The page number is the
            // useful part, so a row that adds nothing over the book's own
            // title shows the page instead.
            let trimmed = row.label.trim();
            if trimmed.is_empty()
                || trimmed == book.manifest.title
                || trimmed
                    .strip_prefix(book.manifest.title.trim())
                    .is_some_and(|rest| !rest.is_empty() && rest.starts_with(' '))
            {
                page_number_of(&row.href)
                    .map(|p| format!("Page {p}"))
                    .unwrap_or_else(|| trimmed.to_string())
            } else {
                row.label.clone()
            }
        } else {
            row.label.clone()
        };
        // A row with no spine index points nowhere — it renders and does
        // nothing rather than sailing to chapter 0. The engine's href rides
        // along for the books the book view renders.
        entries.push(TocEntry {
            chapter: row.spine_index.unwrap_or(usize::MAX),
            label,
            depth: row.depth,
            href: if app.reader.borrow().engine_rendered {
                Some(row.href.clone())
            } else {
                None
            },
        });
    }
    // A book whose TOC is missing still gets a chapter list.
    if entries.is_empty() {
        entries = (0..book.chapters())
            .map(|i| TocEntry {
                chapter: i,
                label: format!("Chapter {}", i + 1),
                depth: 0,
                href: None,
            })
            .collect();
    }
    fill_sidebar(app, &book.manifest.id, &entries, current);
}

/// (Re)fill the sidebar with `entries` for `book_id`, and mark `current`.
fn fill_sidebar(app: &Rc<App>, book_id: &str, entries: &[TocEntry], current: usize) {
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    let tree = build_toc_tree(entries);
    let path = active_path(&tree, current);
    let mut rows = Vec::new();
    let mut groups = Vec::new();
    // One container, emptied and refilled: the layout keeps its widget, the
    // rows belong to whichever book opened last.
    let list = &w.reader.toc_list;
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    append_rows(list, &tree, &mut rows, &mut groups, app, book_id, path.as_deref());
    // The state goes through the App's own cell — the one cell every
    // reader shares, never a Widgets clone's throwaway copy.
    *app.toc_sidebar.borrow_mut() = TocSidebar {
        book_id: book_id.to_string(),
        rows,
        groups,
    };
    // The row the path found takes the mark; the tiles above it are already
    // open, and the mark's own expansion is idempotent with that.
    mark_toc_row(app, current);
}

/// The audiobook's chapters in the same sidebar the books use. The popover
/// this replaces was the player's own; its rows are flat — one per chapter,
/// no nesting — and it is refilled only when the book changes, because the
/// player calls on every chapter move and the rows themselves do not move
/// with it.
pub(crate) fn fill_chapter_rows(app: &Rc<App>, book: &sn_store::books::Book, current: usize) {
    let same_book = app.toc_sidebar.borrow().book_id == book.manifest.id
        && !app.toc_sidebar.borrow().rows.is_empty();
    if !same_book {
        let entries: Vec<TocEntry> = book
            .manifest
            .chapters
            .iter()
            .enumerate()
            .map(|(i, chapter)| TocEntry {
                chapter: i,
                label: chapter.title.clone(),
                depth: 0,
                href: None,
            })
            .collect();
        fill_sidebar(app, &book.manifest.id, &entries, current);
    }
    mark_toc_row(app, current);
}

/// The toolbar's Contents button: slide the sidebar in or out.
pub(crate) fn toc_toggle(app: &Rc<App>) {
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    // No rows yet — no book has opened — nothing to slide in.
    if app.toc_sidebar.borrow().rows.is_empty() {
        return;
    }
    let revealer = &w.reader.toc_revealer;
    revealer.set_reveal_child(!revealer.reveals_child());
}

/// The page number a PDF TOC row's `href` names ("page-233" → 233).
pub(crate) fn page_number_of(href: &str) -> Option<usize> {
    href.strip_prefix("page-").and_then(|n| n.parse().ok())
}

/// The previous/next buttons and their sensitivity.
pub(crate) fn refresh_controls(app: &Rc<App>) {
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    let Ok(Some(book)) = current_book(app) else {
        return;
    };
    let chapter = app.reader.borrow().chapter;
    // The audiobook case never shows the merged toolbar's steppers dead:
    // its chapter count is the manifest's, not the (zero) spine.
    let last = if book.is_audiobook() {
        book.manifest.chapters.len()
    } else {
        book.chapters()
    };
    w.reader.prev.set_sensitive(chapter > 0);
    w.reader.next.set_sensitive(chapter + 1 < last);
}

/// The book the reader has open.
pub fn current_book(
    app: &Rc<App>,
) -> Result<Option<sn_store::books::Book>, sn_store::books::BookError> {
    let id = app.reader.borrow().book_id.clone();
    match id {
        Some(id) => {
            // The cached library first — the reader asks on every control
            // refresh; `find` re-reads every manifest.
            let books = sn_store::books::library_cached(&app.library, &app.library_dirty)?;
            Ok(books.into_iter().find(|b| b.manifest.id == id))
        }
        None => Ok(None),
    }
}

/// Move by `delta` chapters, clamping at both ends.
///
/// For a PDF a chapter is a page, so the buttons are the page steppers: the
/// view scrolls to the page and the voice, if it is speaking, follows.
pub fn go_to_chapter(app: &Rc<App>, delta: i64) {
    let Ok(Some(book)) = current_book(app) else {
        return;
    };
    // An engine book steps without reopening — the relocation reports the
    // new section and everything follows it, the way a PDF's page stepper
    // scrolls instead of re-opening.
    if engine_row_navigates(app) {
        // Read-along steering, strictly while the voice reads along: the
        // steppers move the *voice*, not just the page. The next relocation
        // — the page the move lands on, the next section when the turn
        // crosses one — hands the voice its new start; with no voice
        // engaged a turn is just a turn.
        if app.tts.borrow().is_some() && app.tts_book.get() {
            app.reader.borrow_mut().voice_follows_page = true;
        }
        if let Some(w) = app.widgets.borrow().clone() {
            if delta < 0 {
                w.reader.book.prev();
            } else {
                w.reader.book.next();
            }
        }
        return;
    }
    let is_pdf = book.manifest.format == "pdf";
    // A book's `chapters()` is its spine count — zero for an audiobook,
    // whose chapters are the manifest's. Clamping against the spine left
    // the audiobook's prev/next dead: every press landed on chapter 0.
    let last = if book.is_audiobook() {
        book.manifest.chapters.len()
    } else {
        book.chapters()
    };
    let chapter = app.reader.borrow().chapter as i64 + delta;
    let chapter = chapter.clamp(0, last.saturating_sub(1) as i64) as usize;
    if is_pdf {
        let Some(w) = app.widgets.borrow().clone() else {
            return;
        };
        // In text mode the pages view is hidden, and a hidden view never
        // settles — its page-changed callback would not fire and the text
        // side would stand still while the status line moved on. So text
        // mode refills its own buffer here.
        let text_mode = in_pdf_text_mode(app);
        w.reader.pdf.scroll_to_page(chapter);
        // The scroll's settle handler records the page; the state is moved
        // here too so the buttons and the status are right before the
        // scroll lands.
        {
            let mut state = app.reader.borrow_mut();
            state.chapter = chapter;
        }
        if let Some(w) = app.widgets.borrow().clone() {
            w.reader
                .status
                .set_text(&format!("page {} / {}", chapter + 1, book.chapters()));
            mark_toc_row(app, chapter);
        }
        if text_mode && !app.reader.borrow().speaking {
            // The refill is the worker's read (a cold scan page's
            // recognition costs a second; the UI thread does not wait on
            // it), the same one the visible pages view drives.
            request_page_text(app, &book.manifest.id, chapter);
        }
        refresh_controls(app);
        return;
    }
    // A deck steps without reopening: the whole deck is one document, so a
    // slide turn is a scroll to it — not a re-parse, not a re-layout, and
    // the webview's place survives. The slide's report of the same index is
    // a no-op by equality.
    if book.is_slides() {
        let Some(w) = app.widgets.borrow().clone() else {
            return;
        };
        w.reader.slides.show_slide(chapter);
        {
            let mut state = app.reader.borrow_mut();
            state.chapter = chapter;
        }
        if let Ok(text) = book.chapter_text(chapter) {
            app.reader.borrow_mut().text = text.clone();
            w.reader.slides_text.buffer().set_text(&text);
        }
        w.reader
            .status
            .set_text(&format!("slide {} / {}", chapter + 1, book.chapters()));
        mark_toc_row(app, chapter);
        refresh_controls(app);
        //  the presenter follows a toolbar/pill step too —
        // this path bypasses the page's own slidechanged report.
        presenter_refresh(app);
        return;
    }
    open_book(app, &book.manifest.id, chapter);
}

/// Save the position from the session frontier — the same write-ahead rule
/// the notes player follows: the store receives the frontier, it never feeds
/// one back.
pub fn save_position(app: &Rc<App>, frontier: usize) {
    let Ok(Some(mut book)) = current_book(app) else {
        return;
    };
    let text_len = app.reader.borrow().text.len().max(1);
    book.manifest.position.chapter_index = app.reader.borrow().chapter;
    book.manifest.position.chapter_fraction =
        (frontier as f64 / text_len as f64).clamp(0.0, 1.0);
    book.manifest.last_opened_at = sn_core::AppleDate::now().raw();
    let _ = sn_store::books::BooksStore::save(&book);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(chapter: usize, label: &str, depth: usize) -> TocEntry {
        TocEntry {
            chapter,
            label: label.to_string(),
            depth,
            href: None,
        }
    }

    /// Proust's shape (the azw3 this was built against): parts at depth 0,
    /// chapters at depth 1, sections at depth 2. Each row nests under the
    /// nearest shallower one before it, and a shallow row closes the tile
    /// it belonged to.
    #[test]
    fn a_depth_ladder_nests_each_row_under_its_nearest_shallower_one() {
        let tree = build_toc_tree(&[
            row(0, "Part One", 0),
            row(1, "Combray", 1),
            row(2, "I", 2),
            row(2, "II", 2),
            row(6, "Names", 1),
            row(9, "Part Two", 0),
        ]);
        assert_eq!(tree.len(), 2, "two top-level parts: {tree:?}");
        assert_eq!(tree[0].entry.label, "Part One");
        assert_eq!(tree[0].children.len(), 2, "Combray and Names under it");
        assert_eq!(tree[0].children[0].children.len(), 2, "the two sections");
        assert_eq!(tree[0].children[1].entry.label, "Names");
        assert!(tree[1].children.is_empty(), "Part Two has no children");
    }

    /// A depth jump skips no level — 0 straight to 2 still nests under the
    /// nearest shallower row — and a first row deeper than 0 has no parent,
    /// so it roots at the top rather than vanishing.
    #[test]
    fn a_depth_jump_still_nests_under_the_nearest_shallower_row() {
        let orphaned = build_toc_tree(&[row(3, "Deep start", 2), row(0, "Top", 0)]);
        assert_eq!(orphaned.len(), 2, "no shallower row precedes it");
        let jumped = build_toc_tree(&[row(0, "Top", 0), row(1, "Deep", 2)]);
        assert_eq!(jumped.len(), 1);
        assert_eq!(jumped[0].children[0].entry.label, "Deep");
    }

    /// The active path runs to the leaf naming the open chapter, with the
    /// chain of child indices above it — the widget marks that leaf and
    /// opens exactly the groups on the way.
    #[test]
    fn the_active_path_runs_to_the_leaf_naming_the_open_chapter() {
        let tree = build_toc_tree(&[
            row(0, "Part One", 0),
            row(5, "Combray", 1),
            row(6, "Names", 1),
            row(9, "Part Two", 0),
        ]);
        assert_eq!(active_path(&tree, 6), Some(vec![0, 1]));
        assert_eq!(active_path(&tree, 9), Some(vec![1]));
        assert_eq!(active_path(&tree, 3), None, "no row names chapter 3");
        assert_eq!(active_path(&tree, usize::MAX), None, "a placeholder row is never active");
    }

    /// A group whose own spine index is the open chapter stands in when no
    /// leaf does — the mark lands on the tile's header, which stays visible
    /// even with its children folded away.
    #[test]
    fn a_group_row_stands_in_when_no_leaf_names_the_chapter() {
        let tree = build_toc_tree(&[row(5, "Part One", 0), row(9, "Later", 1)]);
        assert_eq!(active_path(&tree, 5), Some(vec![0]));
        assert_eq!(active_path(&tree, 9), Some(vec![0, 0]));
    }
}

