//! The note editor: fill from the open note, the rendered preview, the
//! pending-edit commit, formatting actions and view switches (split out of
//! `window.rs`, Round 4, pure code-motion).

use std::rc::Rc;

use gtk4::glib;
use gtk4::prelude::*;
use gtk4 as gtk;

use sn_store::model::{Note, Scope};

use crate::app::{App, View, EDIT_DEBOUNCE_MS};

use super::Widgets;
use super::rows::{needs_placeholder_title, row_for};
use super::sidebar::{refresh_all, refresh_list, show_reader_page};

/// Fill the editor from the open note.
pub fn refresh_editor(app: &Rc<App>) {
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    let in_bin = matches!(*app.view.borrow(), View::Bin);
    w.bin_tools.set_visible(in_bin);
    w.delete_button.set_visible(!in_bin);

    let Some(note) = app.open_note() else {
        app.editor_suppress.set(true);
        // The reader's page is a *stack page*, not a note: a book open in the
        // reader must not be replaced by the "no note selected" page when the
        // editor refreshes with nothing open. The check is the reader's own
        // state — a book id in the reader pane — because the editor stack
        // shows whichever of the two the user last asked for. An audiobook
        // keeps its display up here too (stopped included), so the reader
        // chrome stays on rather than flashing the note tools over a cover.
        let reader_has_book = app.reader.borrow().book_id.is_some();
        w.editor_stack
            .set_visible_child_name(if reader_has_book { "reader" } else { "empty" });
        if reader_has_book {
            // The reader page takes the keyboard focus with it (the grab
            // lives in `show_reader_page`, which every route through the
            // reader's own opening path goes through too).
            show_reader_page(app);
        } else {
            // The note toolbar returns: pin/star/export/image were hidden by
            // the reader page and are dead-but-visible on the empty page,
            // exactly as they were before a book was ever opened.
            w.reader.controls.set_visible(false);
            w.pin_button.set_visible(true);
            w.star_button.set_visible(true);
            w.export_button.set_visible(true);
            w.image_button.set_visible(true);
            w.editor_title.set_visible(!app.focus_mode.get());
        }
        w.editor_title.set_text("");
        w.editor_text.buffer().set_text("");
        *app.buffer_note.borrow_mut() = None;
        w.pin_button.set_active(false);
        w.star_button.set_active(false);
        w.editor_status.set_text("");
        w.format_bar.set_visible(false);
        w.preview_toggle.set_visible(false);
        app.editor_suppress.set(false);
        *app.preview_text.borrow_mut() = String::new();
        app.preview_note.borrow_mut().clear();
        clear_preview(&w);
        return;
    };

    // A binned note is read, not edited or rendered: showing live markdown for
    // something in the bin invites editing it back into a state it is not in.
    let show_preview = app.preview.get() && !in_bin;

    // The note page owns the toolbar row: the reader's controls stand down
    // and the note tools return. The focus-mode gate on the two rows the
    // mode collapses directly (the title entry, the format bar) keeps a
    // refresh while the mode is on from re-showing what it hid.
    let focus = app.focus_mode.get();
    w.reader.controls.set_visible(false);
    w.pin_button.set_visible(true);
    w.star_button.set_visible(true);
    w.export_button.set_visible(true);
    w.image_button.set_visible(true);
    w.editor_title.set_visible(!focus);

    // The bar acts on a caret. In the rendered preview and in the bin there
    // is none, so it hides rather than sitting there silently doing nothing.
    w.format_bar.set_visible(!show_preview && !in_bin && !focus);
    w.preview_toggle.set_visible(!in_bin);

    app.editor_suppress.set(true);
    w.preview_toggle.set_active(show_preview);
    w.editor_stack
        .set_visible_child_name(if show_preview { "preview" } else { "editor" });

    // The buffer is the source of truth for the note it holds. Re-filling it
    // on every refresh reset the caret to the end of the note and swallowed
    // the keystrokes not yet committed, so it is refilled only when a
    // *different* note is being shown; pin/star/save refreshes leave the
    // caret exactly where the user left it. The title entry is part of the
    // same truth and gets the same treatment.
    if app.buffer_note.borrow().as_deref() != Some(note.id.as_str()) {
        if needs_placeholder_title(&note) {
            w.editor_title.set_text("");
        } else {
            w.editor_title.set_text(&note.title());
        }
        let buffer = w.editor_text.buffer();
        buffer.set_text(&note.text);
        *app.buffer_note.borrow_mut() = Some(note.id.clone());
    } else {
        // Same note: the title entry may still be stale (a derived title
        // follows the first line of the body). Update it only when the user
        // is not typing in it — a focused entry is the user's caret, not a
        // mirror of the store.
        let shown = if needs_placeholder_title(&note) {
            String::new()
        } else {
            note.title().to_string()
        };
        if !w.editor_title.has_focus() && w.editor_title.text() != shown {
            w.editor_title.set_text(&shown);
        }
    }
    w.pin_button.set_active(note.is_pinned);
    w.star_button.set_active(note.is_favorite);
    app.editor_suppress.set(false);

    if show_preview {
        render_preview(app, &w, &note);
    }

    let status = if in_bin {
        let deleted = note
            .deleted_at
            .map(|d| d.local_day())
            .unwrap_or_default();
        format!(
            "{} words · ~{} min listen · binned {deleted} · kept 30 days",
            note.word_count(),
            note.listen_minutes()
        )
    } else {
        format!(
            "{} words · ~{} min listen · edited {}",
            note.word_count(),
            note.listen_minutes(),
            note.updated_at.local_day()
        )
    };
    // A save that failed is the one thing the user must be told: without
    // this, a full disk or a read-only directory lost the note with nothing
    // on screen to say so. The error is reported once, then the status line
    // goes back to normal.
    let status = match app.notes.take_save_error() {
        Some(why) => format!("⚠ Not saved: {why}"),
        None => status,
    };
    w.editor_status.set_text(&status);
}

/// Render a note's markdown into the preview pane.
///
/// Memoised on the note text: the list rebuilds on every debounced save, and
/// re-parsing and re-building a widget tree for a note whose text has not
/// moved is pure cost on the keystroke path.
///
/// A remote image is *not* fetched here. The render pass records the URLs it
/// could not show, this function pulls them off the UI thread through the same
/// channel-and-timeout idiom the engine load uses, and a fetch that landed
/// drops the memo so the next pass draws the pictures. One round trip per URL
/// per session: `app.remote_seen` records successes and failures alike, so a
/// URL that keeps failing shows a placeholder instead of retrying on every
/// keystroke.
pub(crate) fn render_preview(app: &Rc<App>, w: &Widgets, note: &Note) {
    let same_pane = *app.preview_text.borrow() == note.text
        && app.preview_note.borrow().as_str() == note.id.as_str();
    if same_pane {
        return;
    }
    *app.preview_note.borrow_mut() = note.id.clone();
    *app.preview_text.borrow_mut() = note.text.clone();

    let mut ctx = crate::render::RenderCtx::with_seen(&note.id, app.remote_seen.clone());
    // The failed set is the window's own: the fetch poll fills it, this
    // pass reads it, and a dead link shows its failure placeholder instead
    // of a spinner that never resolves.
    ctx.failed_remote = app.remote_failed.clone();
    let ctx = std::rc::Rc::new(ctx);
    let t_parse = std::time::Instant::now();
    let doc = std::rc::Rc::new(sn_format::parse_markdown(&note.text));
    tracing::info!(
        id = %note.id,
        chars = note.text.len(),
        blocks = doc.blocks.len(),
        elapsed_ms = t_parse.elapsed().as_secs_f64() * 1000.0,
        "preview parsed"
    );

    clear_preview(w);
    // The tree is built in chunks: the first screen paints now, the tail
    // follows on idle callbacks while the user reads. The remote-image
    // fetch is armed when the whole document is in — a URL in the last
    // paragraph must be queued too.
    let app_for_fetch = app.clone();
    let view = crate::render::render_document_progressive(
        doc,
        ctx.clone(),
        Box::new(move |ctx: &crate::render::RenderCtx| {
            fetch_remote_images(&app_for_fetch, ctx);
        }),
    );
    w.preview_box.append(&view);
    // A freshly built scroller starts at the top, which is what this pane
    // wants: re-rendering while scrolled to the bottom would otherwise snap
    // the view back on every save. (An earlier version forced a fake
    // Adjustment with upper=1.0 onto the scroller here; overriding the
    // viewport's adjustment desynchronises the scrollbar from the content and
    // breaks scrolling outright.)
    fetch_remote_images(app, &ctx);
}

/// Pull the URLs the pass just queued off the UI thread.
///
/// The worker touches no widget: it fetches each image into the note's
/// `remote/` cache and reports, per URL, whether it landed. The poll runs on
/// the UI thread — a background thread cannot call `idle_add_local` — and
/// *any* outcome (success or failure) drops the memo so the next refresh
/// draws every picture the queue produced and turns the spinners of the
/// URLs that failed into failure placeholders. Reporting only the successes
/// is the Android build's bug: a dead link spins forever. A note already
/// fully cached costs one empty poll.
pub(crate) fn fetch_remote_images(app: &Rc<App>, ctx: &crate::render::RenderCtx) {
    let urls: Vec<String> = ctx.pending_remote.borrow().clone();
    if urls.is_empty() {
        return;
    }
    // `timeout_add_local` takes a `'static` closure, so the poll gets its own
    // handle rather than a borrow of the caller's — the same shape as the
    // engine load's poll, and the reason every other tick in this file clones
    // the `Rc` before going into a closure.
    let app = app.clone();
    let images_dir = ctx
        .images_dir
        .clone()
        .unwrap_or_else(sn_core::paths::note_images_dir);
    let note_id = ctx.note_id.clone().unwrap_or_default();
    let (tx, rx) = std::sync::mpsc::channel::<Vec<(String, bool)>>();
    std::thread::spawn(move || {
        let cache_dir = images_dir.join(&note_id).join("remote");
        let mut results = Vec::with_capacity(urls.len());
        for url in &urls {
            let ok = sn_net::fetch_image(url, &cache_dir, sn_net::MAX_IMAGE_BYTES).is_ok();
            results.push((url.clone(), ok));
        }
        // A worker that panicked mid-loop leaves the sender dropped, which the
        // poll reads as "no more answers" rather than hanging on it.
        let _ = tx.send(results);
    });
    let rx = std::cell::RefCell::new(rx);
    glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
        match rx.borrow_mut().try_recv() {
            Ok(results) => {
                let mut failed = app.remote_failed.borrow_mut();
                let mut landed = 0usize;
                for (url, ok) in results {
                    if ok {
                        landed += 1;
                    } else {
                        failed.insert(url);
                    }
                }
                drop(failed);
                tracing::debug!(landed, "the background image fetch reported");
                // The memo is what stands between the fetched files and a
                // picture: drop it and the next refresh re-renders — with the
                // failures now in the set, the same refresh retires their
                // spinners.
                app.preview_text.borrow_mut().clear();
                app.preview_note.borrow_mut().clear();
                refresh_editor(&app);
                glib::ControlFlow::Break
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => glib::ControlFlow::Break,
        }
    });
}

pub(crate) fn clear_preview(w: &Widgets) {
    while let Some(child) = w.preview_box.first_child() {
        w.preview_box.remove(&child);
    }
}

/// A formatting-bar action. The button carries only this; the meaning lives in
/// `crate::format`, so a keyboard shortcut and a button press cannot diverge.
#[derive(Clone, Copy, Debug)]
pub(crate) enum FormatAction {
    /// Wrap or unwrap the selection in a marker.
    Wrap(&'static str),
    /// Add or remove a line prefix.
    Line(&'static str),
    Table,
    Link,
}

/// Run a formatting action against the open note's buffer.
pub(crate) fn apply_format_action(app: &Rc<App>, action: &FormatAction) {
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    // Formatting the source text is meaningless while the rendered preview is
    // the visible pane — there is no caret to act on.
    if app.preview.get() {
        return;
    }
    if matches!(*app.view.borrow(), View::Bin) {
        return;
    }
    let buffer = w.editor_text.buffer();
    let (text, sel) = crate::format::text_and_selection(&buffer);

    let edit = match action {
        FormatAction::Wrap(m) => crate::format::toggle_wrap(&text, sel, m),
        FormatAction::Line(p) => crate::format::toggle_line_prefix(&text, sel, p),
        FormatAction::Table => crate::format::insert_block(&text, sel, &crate::format::table_skeleton()),
        FormatAction::Link => crate::format::insert_link(&text, sel),
    };
    edit.apply(&buffer);
    w.editor_text.grab_focus();
}

pub(crate) fn set_view(app: &Rc<App>, view: View) {
    if *app.view.borrow() == view {
        return;
    }
    // Sidebar navigation must not eat the tail of the note being left.
    commit_pending_edit(app);
    remember_view(app, &view);
    *app.view.borrow_mut() = view;
    *app.open_note.borrow_mut() = None;
    // The list key embeds the view, so this always rebuilds.
    *app.list_key.borrow_mut() = String::new();
    refresh_all(app);
}

/// The sidebar section is a preference, not a default: the window opens on
/// the section the user last had open. The Notes scopes also update
/// `activeNotebookScope` — the key the family's other apps read and write —
/// while Bin and Books are this app's alone, so they live in `lastView`.
pub(crate) fn remember_view(app: &Rc<App>, view: &View) {
    let (pref, scope) = match view {
        View::Notes(Scope::All) => ("all".to_string(), Some(Scope::All)),
        View::Notes(Scope::Unfiled) => ("unfiled".to_string(), Some(Scope::Unfiled)),
        View::Notes(Scope::Notebook(id)) => (
            format!("notebook:{id}"),
            Some(Scope::Notebook(id.clone())),
        ),
        View::Bin => ("bin".to_string(), None),
        View::Books => ("books".to_string(), None),
    };
    app.prefs.update(move |p| {
        p.last_view = pref.clone();
        if let Some(scope) = scope.clone() {
            p.set_scope(scope);
        }
    });
}
// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

pub(crate) fn new_note(app: &Rc<App>) {
    // The buffer still belongs to the previous note; commit its tail while it
    // does, or the keystrokes before Ctrl+N never land.
    commit_pending_edit(app);
    let scope = match &*app.view.borrow() {
        View::Notes(Scope::Notebook(id)) => Some(id.clone()),
        _ => None,
    };
    let note = app.notes.create(scope);
    *app.open_note.borrow_mut() = Some(note.id.clone());
    refresh_all(app);
    focus_editor(app);
}

pub(crate) fn open_note(app: &Rc<App>, id: &str) {
    if app.open_note.borrow().as_deref() == Some(id) {
        return;
    }
    // Flush any pending edit for the previous note before switching.
    commit_pending_edit(app);
    *app.open_note.borrow_mut() = Some(id.to_string());
    // A note opens the way `renderMarkdown` says: reading mode when the
    // pref is on, the editor when it is off. The eye button still overrides
    // per visit — and writes the pref, so the next note follows.
    app.preview.set(app.prefs.get().render_markdown);
    refresh_editor(app);
    // Move the list highlight onto the note just opened. The commit above may
    // have rebuilt the list while the outgoing note was still the open one,
    // which would leave the old row highlighted; and a full rebuild here is
    // both unnecessary and unreliable — the memo skips it entirely when
    // nothing was committed. Reselect under the suppress flag so the click
    // does not re-enter this handler.
    if let Some(row) = row_for(app, id) {
        app.selection_suppress.set(true);
        let w = app.widgets.borrow().clone();
        if let Some(w) = w {
            w.notes_list.select_row(Some(&row));
        }
        app.selection_suppress.set(false);
    }
}

/// Commit whatever the editor owes: buffer text and title entry, once each.
///
/// Both are read from the widgets *now*, under one rule: a pending edit belongs
/// to the note that was open while the user was typing, so it commits only if
/// that note is still the open one. A note that left the editor by another
/// route (deleted, purged) drops its pending edit — the user threw the note
/// away after typing, and the typed text must not chase it into the bin.
///
/// Every path that swaps or clears the open note therefore calls this FIRST,
/// while the buffer still holds the outgoing note's text: `open_note`,
/// `set_view`, `new_note`, the restore button, the resume offer.
pub(crate) fn commit_pending_edit(app: &Rc<App>) {
    let open = app.open_note.borrow().clone();
    let mut dirty = false;

    if let Some(id) = app.pending_edit.borrow_mut().take() {
        if open.as_deref() == Some(id.as_str()) {
            if let Some(w) = app.widgets.borrow().clone() {
                let buf = w.editor_text.buffer();
                let text = buf.text(&buf.start_iter(), &buf.end_iter(), true).to_string();
                app.notes.update_text(&id, &text);
                dirty = true;
            }
        }
    }
    if let Some(id) = app.pending_title.borrow_mut().take() {
        if open.as_deref() == Some(id.as_str()) {
            if let Some(w) = app.widgets.borrow().clone() {
                let title = w.editor_title.text().to_string();
                app.notes.set_title(&id, &title);
                dirty = true;
            }
        }
    }
    if dirty {
        // The list preview and status derive from the text and title.
        refresh_list(app);
        // The editor's own status line carries the save result, so it has to
        // be repainted: a write that failed shows there, not in a log the
        // user never sees.
        refresh_editor(app);
    }
}

/// (Re)arm the single debounce that commits editor changes.
///
/// One timer per burst: replace any pending one, so fast typing never queues a
/// pile of timers. Both the buffer and the title route through it, so a burst
/// of mixed typing and titling costs one commit and one list rebuild.
pub(crate) fn arm_commit_timer(app: &Rc<App>) {
    if let Some(old) = app.pending_source.borrow_mut().take() {
        old.remove();
    }
    let app2 = app.clone();
    let source = glib::timeout_add_local(
        std::time::Duration::from_millis(EDIT_DEBOUNCE_MS),
        move || {
            commit_pending_edit(&app2);
            glib::ControlFlow::Break
        },
    );
    *app.pending_source.borrow_mut() = Some(source);
}

pub(crate) fn focus_editor(app: &Rc<App>) {
    if let Some(w) = app.widgets.borrow().clone() {
        w.editor_text.grab_focus();
    }
}

// ---------------------------------------------------------------------------
// Editor buffer
// ---------------------------------------------------------------------------

pub(crate) fn buffer_changed(app: &Rc<App>, _buf: &gtk::TextBuffer) {
    if app.editor_suppress.get() {
        return;
    }
    let Some(id) = app.open_note.borrow().clone() else {
        return;
    };
    // O(1) per keystroke: record which note changed and let the debounce read
    // the buffer once. Copying the whole text here made every key cost an
    // O(note-length) walk and allocation before the debounce even ran.
    *app.pending_edit.borrow_mut() = Some(id);
    arm_commit_timer(app);
}