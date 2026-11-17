//! Dialogs: confirmations, prompts, and the resume offer.
//!
//! GTK 4.10 deprecated `MessageDialog` and `Dialog` in favour of `AlertDialog`.
//! Confirmations and the resume offer use `AlertDialog`, which is the modern
//! answer and has no window of its own.
//!
//! `Dialog` is still used where a real widget has to sit inside the dialog —
//! the notebook-name prompt's entry, the BookDrop peer chooser's and the JEX
//! export notebook chooser's DropDowns — and GTK 4.14 offers no replacement
//! for that. The deprecation warning is allowed at module level rather than
//! scattered, so the build's warning list only ever shows something new.
//!
//! One rule throughout: **a destructive action asks first, and the answer is
//! carried by the button that was clicked**, not by a shared "confirmed" flag
//! that a second click could disturb.

#![allow(deprecated)]

use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;

use crate::app::App;


/// Ask a yes/no question. `on_confirm` runs only on the affirmative button.
///
/// GTK 4.14's `AlertDialog` answers through `choose()`, which returns the
/// index of the button the user pressed. That index is the *only* thing that
/// decides, so there is no shared "was confirmed" state for a second click to
/// disturb.
pub fn confirm(
    app: &Rc<App>,
    title: &str,
    body: &str,
    confirm_label: &str,
    destructive: bool,
    on_confirm: impl FnOnce(&Rc<App>) + 'static,
) {
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    let dialog = gtk::AlertDialog::builder()
        .message(title)
        .detail(body)
        .modal(true)
        .build();
    // Button order is the response value: index 0 is Cancel, index 1 the
    // affirmative. Two buttons, always, in that order.
    let buttons = if destructive {
        vec!["Cancel", confirm_label]
    } else {
        vec!["Cancel", "OK"]
    };
    dialog.set_buttons(&buttons);
    // `-1` means "use the default", which GTK resolves to the last button.
    dialog.set_default_button(-1);
    // The cancel response is `-1` in GTK's convention.
    dialog.set_cancel_button(-1);

    let app = app.clone();
    dialog.choose(Some(&w.window), None::<&gtk::gio::Cancellable>, move |result| {
        if let Ok(index) = result {
            if index == 1 {
                on_confirm(&app);
            }
        }
    });
}

/// Ask for one line of text. `on_ok` receives the trimmed value.
///
/// One dialog, one answer: `on_ok` is stored in an `Option` and taken on the
/// first response that carries a value, so Enter-in-the-entry and the OK button
/// cannot both fire it. The old shape — an `activate` handler *and* a response
/// handler, each calling `on_ok` — was an `FnOnce` captured twice, which is
/// exactly the kind of double-commit this rewrite is supposed to avoid.
pub fn prompt(
    app: &Rc<App>,
    title: &str,
    placeholder: &str,
    on_ok: impl FnOnce(&Rc<App>, String) + 'static,
) {
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    let dialog = gtk::Dialog::builder()
        .title(title)
        .transient_for(&w.window)
        .modal(true)
        .use_header_bar(1)
        .build();

    let entry = gtk::Entry::builder().placeholder_text(placeholder).build();
    let content = dialog.content_area();
    content.set_spacing(8);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.append(&entry);

    dialog.add_button("Cancel", gtk::ResponseType::Cancel);
    dialog.add_button("OK", gtk::ResponseType::Ok);
    entry.set_activates_default(true);
    dialog.set_default_response(gtk::ResponseType::Ok);

    // `Rc<RefCell<..>>` because GTK signals are `Fn`, and only one of them may
    // consume the callback.
    let on_ok = std::rc::Rc::new(std::cell::RefCell::new(Some(on_ok)));
    let app = app.clone();
    let entry_for_response = entry.clone();
    dialog.connect_response(move |d, response| {
        if response == gtk::ResponseType::Ok {
            let text = entry_for_response.text().trim().to_string();
            if !text.is_empty() {
                if let Some(cb) = on_ok.borrow_mut().take() {
                    cb(&app, text);
                }
            }
        }
        d.close();
    });
    dialog.present();
    entry.grab_focus();
}

/// Move a note to the bin, after confirmation.
pub fn confirm_delete(app: &Rc<App>, id: &str) {
    let title = app
        .notes
        .get(id)
        .map(|n| n.title())
        .unwrap_or_else(|| "this note".into());
    let id = id.to_string();
    confirm(
        app,
        &format!("Move “{title}” to the bin?"),
        "It stays recoverable for 30 days, then is deleted permanently.",
        "Move to bin",
        false,
        move |app| {
            app.notes.delete(&id);
            *app.open_note.borrow_mut() = None;
            crate::window::refresh_all(app);
        },
    );
}

/// Delete a book, after confirmation. The one path the shelf's right-click
/// and the reader's toolbar button share.
///
/// Books have no bin: the store's delete removes the book's whole directory
/// — the original file (kept verbatim on import by every format lane), the
/// cover, the manifest and the reading position. The dialog says that
/// plainly instead of implying something recoverable.
pub fn confirm_delete_book(app: &Rc<App>, book: sn_store::books::Book) {
    let title = book.manifest.title.clone();
    let id = book.manifest.id.clone();
    confirm(
        app,
        &format!("Delete “{title}”?"),
        "The book, its original file and your place in it are removed for good. \
         Your notes are untouched.",
        "Delete book",
        true,
        move |app| {
            // A book open in the reader must not stay open on a manifest
            // that no longer exists — the next refresh would try to read it
            // back and the reader would be looking at nothing.
            let was_open = app.reader.borrow().book_id.as_deref() == Some(id.as_str());
            if let Err(why) = sn_store::books::BooksStore::delete(&book) {
                crate::window::report_failure(
                    app,
                    &format!("Could not delete “{title}”: {why}"),
                );
                return;
            }
            if was_open {
                crate::reader::forget_open_book(app);
            }
            // The library cache holds the deleted book until it is marked
            // dirty; refresh_all reads through it, so mark it first.
            app.library_dirty.set(true);
            crate::window::refresh_all(app);
        },
    );
}

/// Delete a note permanently, after confirmation.
pub fn confirm_purge(app: &Rc<App>, id: &str) {
    let title = app
        .notes
        .get(id)
        .map(|n| n.title())
        .unwrap_or_else(|| "this note".into());
    let id = id.to_string();
    confirm(
        app,
        &format!("Delete “{title}” permanently?"),
        "This cannot be undone.",
        "Delete forever",
        true,
        move |app| {
            if let Some(purged) = app.notes.purge(&id) {
                app.bookmarks.remove_note(&purged);
                // Note images belong to the note; they go with it.
                let dir = sn_core::paths::note_images_dir().join(&purged);
                let _ = std::fs::remove_dir_all(dir);
            }
            *app.open_note.borrow_mut() = None;
            crate::window::refresh_all(app);
        },
    );
}

/// Empty the whole bin, after confirmation. Says how many notes are at stake.
pub fn confirm_empty_bin(app: &Rc<App>) {
    let count = app.notes.binned_active().len();
    if count == 0 {
        return;
    }
    confirm(
        app,
        &format!("Delete {count} note{} permanently?", plural(count)),
        "This cannot be undone. Notes older than 30 days are already gone.",
        "Delete all",
        true,
        |app| {
            for purged in app.notes.empty_bin() {
                app.bookmarks.remove_note(&purged);
                let dir = sn_core::paths::note_images_dir().join(&purged);
                let _ = std::fs::remove_dir_all(dir);
            }
            crate::window::refresh_all(app);
        },
    );
}

/// Ask for a notebook name and create it.
pub fn new_notebook(app: &Rc<App>) {
    prompt(app, "New notebook", "Notebook name", |app, name| {
        app.notebooks.create(&name);
        crate::window::refresh_all(app);
    });
}

// ---------------------------------------------------------------------------
// BookDrop: the in-app send picker
// ---------------------------------------------------------------------------

/// What one row of the send picker stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Sendable {
    /// A file that already lies inside the app's own storage — the library's
    /// book copy or an export this app's pipelines wrote. Sent as it lies.
    File {
        path: std::path::PathBuf,
        file_name: String,
        label: String,
    },
    /// A note, packaged when the send runs: one Markdown file per note, or
    /// the whole library as one `.jex` archive.
    Note { id: String, label: String },
}

/// A labelled section of the picker's list.
pub(crate) struct SendableGroup {
    pub section: &'static str,
    pub rows: Vec<Sendable>,
}

/// The extensions the receiver's import router accepts (`bookdrop::route`).
/// A file the app holds but no importer on the far end reads is not offered:
/// it would land as a rejected transfer.
const ROUTER_ACCEPTS: &[&str] = &[
    "jex", "md", "markdown", "txt", "text", "docx", "wav", "epub", "pdf", "pptx", "ppsx", "potx",
    "ppt", "odp", "m4b", "m4a", "mp4", "mp3", "opus", "ogg", "oga", "mobi", "azw", "azw3", "prc",
    "pdb", "fb2", "rtf", "html", "htm", "htmlz",
];

/// What a library book's format is called in the picker's rows.
fn book_kind(format: &str) -> &'static str {
    match format {
        "m4b" | "opus" => "audiobook",
        "epub" => "EPUB",
        "pdf" => "PDF",
        "pptx" | "ppsx" | "potx" | "ppt" | "odp" => "slides",
        // The headless documents, by their family: a row reads
        // "Pocket Pediatrics · Kindle" rather than "· book".
        "mobi" | "azw" | "azw3" | "prc" | "pdb" => "Kindle",
        "fb2" => "FictionBook",
        "rtf" => "RTF",
        "html" | "htm" | "htmlz" => "HTML",
        "txt" | "text" => "text",
        _ => "book",
    }
}

/// Everything this app can send, grouped the way the picker shows it: the
/// library's books, the notes, and the files this app's own pipelines have
/// exported. Nothing outside the app's data is offered — BookDrop sends from
/// within the app.
pub(crate) fn sendable_items(app: &Rc<App>) -> Vec<SendableGroup> {
    let mut groups = Vec::new();

    // The library: every book's own copy of its file — the audio for an
    // audiobook, the archive copy for everything else — named as the sender
    // named it, so the receiver's history row reads like the file did.
    let mut library = Vec::new();
    if let Ok(books) = sn_store::books::BooksStore::all() {
        for book in books {
            let path = if book.is_audiobook() {
                book.audio()
            } else {
                book.original()
            };
            if !path.exists() {
                continue;
            }
            let file_name = if book.manifest.original_file_name.is_empty() {
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "book".to_string())
            } else {
                book.manifest.original_file_name.clone()
            };
            let label = format!(
                "{}  ·  {}",
                book.manifest.title,
                book_kind(&book.manifest.format)
            );
            library.push(Sendable::File {
                path,
                file_name,
                label,
            });
        }
    }
    if !library.is_empty() {
        groups.push(SendableGroup {
            section: "Library",
            rows: library,
        });
    }

    let notes: Vec<Sendable> = app
        .notes
        .active()
        .into_iter()
        .map(|note| Sendable::Note {
            id: note.id.clone(),
            label: note.title().to_string(),
        })
        .collect();
    if !notes.is_empty() {
        groups.push(SendableGroup {
            section: "Notes",
            rows: notes,
        });
    }

    // The files this app has already written out (recordings, archives,
    // exports): sent as they lie, like the library's.
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(sn_core::paths::exports_dir()) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            if !ROUTER_ACCEPTS.contains(&ext.as_str()) {
                continue;
            }
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            files.push(Sendable::File {
                file_name: name.clone(),
                path,
                label: name,
            });
        }
    }
    if !files.is_empty() {
        groups.push(SendableGroup {
            section: "Exported files",
            rows: files,
        });
    }

    groups
}

/// Turn the ticked rows into the sender's item list. Files go as they lie;
/// the notes are staged in the app's own exports folder first — as Markdown
/// files, one per note, or as one `.jex` archive of the whole library (the
/// archive covers everything, so the note ticks stand aside for it).
fn resolve_sendables(
    app: &Rc<App>,
    checked: &[&Sendable],
    as_jex: bool,
) -> Vec<sn_net::sender::SendItem> {
    let staging = sn_core::paths::exports_dir().join("bookdrop");
    let mut items = Vec::new();
    for item in checked {
        if let Sendable::File { path, file_name, .. } = item {
            if path.exists() {
                items.push(sn_net::sender::SendItem {
                    path: path.clone(),
                    file_name: file_name.clone(),
                });
            }
        }
    }
    if as_jex {
        let bytes = crate::export::jex_archive(&app.notes, &app.notebooks, None);
        let name = crate::export::jex_suggested_name("library");
        let path = staging.join(&name);
        if crate::export::write_file(&path, &bytes).is_ok() {
            items.push(sn_net::sender::SendItem {
                path,
                file_name: name,
            });
        }
    } else {
        let active = app.notes.active();
        for (index, note) in active.iter().enumerate() {
            let ticked = checked
                .iter()
                .any(|s| matches!(s, Sendable::Note { id, .. } if id == &note.id));
            if !ticked {
                continue;
            }
            let body = crate::export::one(note, crate::export::Format::Markdown);
            let name = format!("{}.md", crate::export::slug_for(note, index));
            let path = staging.join(&name);
            if crate::export::write_file(&path, body.as_bytes()).is_ok() {
                items.push(sn_net::sender::SendItem {
                    path,
                    file_name: name,
                });
            }
        }
    }
    items
}

/// The in-app send picker: everything the app can send with a tick box, the
/// notes' packaging, and a Send that hands the resolved files to `on_ready`.
///
/// This is where the old system file dialog used to sit: sending picked
/// files from the desktop's file manager (Cosmic Files, via the portal),
/// which put BookDrop's send half outside the app it serves. The picker
/// below lists only what the app already holds — the same rule the receive
/// half keeps, which saves what lands into the app's library.
pub(crate) fn pick_bookdrop_items(
    app: &Rc<App>,
    on_ready: impl Fn(Vec<sn_net::sender::SendItem>) + 'static,
) {
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    let groups = sendable_items(app);
    if groups.iter().all(|g| g.rows.is_empty()) {
        crate::window::report_failure(
            app,
            "Nothing to send yet — the library and notes are empty.",
        );
        return;
    }
    let dialog = gtk::Dialog::builder()
        .title("Send from Speechnotes")
        .transient_for(&w.window)
        .modal(true)
        .use_header_bar(1)
        .default_width(480)
        .default_height(560)
        .build();
    let content = dialog.content_area();
    content.set_spacing(8);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.append(
        &gtk::Label::builder()
            .label("Everything this app can send — tick what goes:")
            .halign(gtk::Align::Start)
            .build(),
    );

    let list = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .build();
    let mut checks: Vec<(Sendable, gtk::CheckButton)> = Vec::new();
    for group in groups {
        list.append(
            &gtk::Label::builder()
                .label(group.section)
                .halign(gtk::Align::Start)
                .css_classes(["row-meta"])
                .build(),
        );
        for row in group.rows {
            let label = match &row {
                Sendable::File { label, .. } => label.clone(),
                Sendable::Note { label, .. } => label.clone(),
            };
            let check = gtk::CheckButton::with_label(&label);
            check.set_halign(gtk::Align::Start);
            list.append(&check);
            checks.push((row, check));
        }
    }
    // A library of 70 books must not stretch the dialog to the screen — the
    // same scroller the chapter menu uses.
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .min_content_height(340)
        .max_content_height(440)
        .propagate_natural_height(true)
        .child(&list)
        .build();
    content.append(&scroll);

    // The notes' packaging. "Markdown" writes one file per ticked note (the
    // receiver's router imports each straight back in); the archive is the
    // whole library at once, notebooks and images with it.
    let packaging = gtk::DropDown::from_strings(&[
        "Markdown file per note",
        "One .jex archive of the whole library",
    ]);
    let pack_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    pack_row.append(&gtk::Label::new(Some("Notes as")));
    pack_row.append(&packaging);
    content.append(&pack_row);

    dialog.add_button("Cancel", gtk::ResponseType::Cancel);
    dialog.add_button("Send", gtk::ResponseType::Ok);
    dialog.set_default_response(gtk::ResponseType::Ok);

    let checks = std::rc::Rc::new(checks);
    let packaging = std::rc::Rc::new(packaging);
    let on_ready = std::rc::Rc::new(on_ready);
    let app = app.clone();
    dialog.connect_response(move |d, response| {
        d.close();
        if response != gtk::ResponseType::Ok {
            return;
        }
        let checked: Vec<&Sendable> = checks
            .iter()
            .filter(|(_, check)| check.is_active())
            .map(|(item, _)| item)
            .collect();
        if checked.is_empty() {
            return;
        }
        let as_jex = packaging.selected() == 1;
        let items = resolve_sendables(&app, &checked, as_jex);
        if !items.is_empty() {
            on_ready(items);
        }
    });
    dialog.present();
}


/// Choose a notebook (or the whole library), pick where the `.jex` goes, and
/// build the archive on a worker.
//  authored for the JEX interchange section. Moved
// here from settings.rs 2026-10-31 (beta) with the other choosers; body
// unchanged.
pub fn pick_jex_export(app: &Rc<App>) {
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    let notebooks = app.notebooks.all();
    if notebooks.is_empty() && app.notes.active().is_empty() {
        crate::reader::set_status(app, "Nothing to export yet");
        return;
    }
    let dialog = gtk::Dialog::builder()
        .title("Export as JEX")
        .transient_for(&w.window)
        .modal(true)
        .use_header_bar(1)
        .build();
    let content = dialog.content_area();
    content.set_spacing(8);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    let label = gtk::Label::new(Some("Which notebook?"));
    content.append(&label);
    // Index 0 is the whole library; every notebook follows in store order.
    let mut names = vec!["Everything (all notes)".to_string()];
    names.extend(notebooks.iter().map(|nb| nb.name.clone()));
    let dropdown = gtk::DropDown::from_strings(
        names.iter().map(|s| s.as_str()).collect::<Vec<_>>().as_slice(),
    );
    content.append(&dropdown);
    dialog.add_button("Cancel", gtk::ResponseType::Cancel);
    dialog.add_button("Export", gtk::ResponseType::Ok);
    dialog.set_default_response(gtk::ResponseType::Ok);

    let app = app.clone();
    let notebooks = std::rc::Rc::new(notebooks);
    dialog.connect_response(move |d, response| {
        if response == gtk::ResponseType::Ok {
            let chosen = dropdown.selected() as usize;
            // Close the chooser before the save dialog opens so the two
            // transient windows never stack.
            d.close();
            let notebook_id = if chosen == 0 {
                None
            } else {
                notebooks.get(chosen - 1).map(|nb| nb.id.clone())
            };
            let suggested = crate::export::jex_suggested_name(if chosen == 0 {
                "library"
            } else {
                notebooks
                    .get(chosen - 1)
                    .map(|nb| nb.name.as_str())
                    .unwrap_or("notebook")
            });
            crate::settings::save_jex_export(&app, notebook_id, &suggested);
        } else {
            d.close();
        }
    });
    dialog.present();
}

/// Offer to reopen the most recent bookmarked note.
pub fn offer_resume(app: &Rc<App>) {
    let Some((key, bookmark)) = app.bookmarks.resume_candidate(crate::app::RESUME_WINDOW) else {
        return;
    };
    let Some(note_id) = key.strip_prefix("note:").map(str::to_string) else {
        return;
    };
    let Some(note) = app.notes.get(&note_id) else {
        return;
    };
    // A stale bookmark must not resume into text it was never taken against.
    if !bookmark.matches_text(&note.text) {
        return;
    }
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    let dialog = gtk::AlertDialog::builder()
        .message(&format!("Continue “{}”?", note.title()))
        .detail(&format!("You stopped {} ago.", human_ago(bookmark.saved_at)))
        .modal(true)
        .build();
    dialog.set_buttons(&["Not now", "Reopen note"]);
    dialog.set_default_button(-1);
    dialog.set_cancel_button(-1);

    let app = app.clone();
    dialog.choose(Some(&w.window), None::<&gtk::gio::Cancellable>, move |result| {
        if let Ok(index) = result {
            if index == 1 {
                *app.open_note.borrow_mut() = Some(note_id.clone());
                crate::window::refresh_all(&app);
            }
        }
    });
}

fn human_ago(when: sn_core::AppleDate) -> String {
    let secs = sn_core::AppleDate::now().raw() - when.raw();
    if !(secs > 60.0) {
        return "moments".into();
    }
    match secs as i64 {
        s if s < 3600 => format!("{} min", s / 60),
        s if s < 86_400 => format!("{} h", s / 3600),
        s => format!("{} days", s / 86_400),
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    /// The picker only offers files the receiving end can import: every
    /// extension in the list must be one `bookdrop::route` really routes,
    /// so a ticked row can never land as a rejected transfer.
    #[test]
    fn router_accepts_covers_the_route_table() {
        let routed = [
            "jex", "md", "markdown", "txt", "text", "docx", "wav", "epub", "pdf", "pptx", "ppsx",
            "potx", "ppt", "odp", "m4b", "m4a", "mp4", "mp3", "opus", "ogg", "oga", "mobi", "azw",
            "azw3", "prc", "pdb", "fb2", "rtf", "html", "htm", "htmlz",
        ];
        for ext in routed {
            assert!(
                ROUTER_ACCEPTS.contains(&ext),
                "route() accepts .{ext} but the picker does not offer it"
            );
        }
        // And nothing offered is dead weight: the list is exactly the table.
        for offered in ROUTER_ACCEPTS {
            assert!(
                routed.contains(offered),
                "the picker offers .{offered} but route() has no importer for it"
            );
        }
    }

    #[test]
    fn book_kinds_name_the_formats_the_library_holds() {
        assert_eq!(book_kind("m4b"), "audiobook");
        assert_eq!(book_kind("opus"), "audiobook");
        assert_eq!(book_kind("epub"), "EPUB");
        assert_eq!(book_kind("pdf"), "PDF");
        assert_eq!(book_kind("pptx"), "slides");
        assert_eq!(book_kind("odp"), "slides");
        assert_eq!(book_kind("azw3"), "Kindle");
        assert_eq!(book_kind("mobi"), "Kindle");
        assert_eq!(book_kind("pdb"), "Kindle");
        assert_eq!(book_kind("fb2"), "FictionBook");
        assert_eq!(book_kind("rtf"), "RTF");
        assert_eq!(book_kind("htmlz"), "HTML");
        assert_eq!(book_kind("txt"), "text");
    }
}
