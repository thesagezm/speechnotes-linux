//! Export the open note and insert an image into it (split out of
//! `window.rs`, Round 4, pure code-motion).

use std::rc::Rc;

use gtk4::prelude::*;
use gtk4 as gtk;

use crate::app::App;

use super::editor::commit_pending_edit;
use super::report_failure;

// ---------------------------------------------------------------------------
// Export and images
// ---------------------------------------------------------------------------

/// Ask where to save the open note, then write it.
///
/// The dialog is `gtk::FileDialog`, not the deprecated `FileChooserDialog`, and
/// the write happens in its callback: a dialog that owns the write can only ever
/// finish once the user answers, which is the right shape for a save.
pub(crate) fn export_open_note(app: &Rc<App>) {
    let Some(note) = app.open_note() else {
        return;
    };
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };

    let dialog = gtk::FileDialog::new();
    dialog.set_title("Export note");
    let suggested = crate::export::suggested_name(&note, crate::export::Format::Markdown);
    dialog.set_initial_name(Some(&suggested));
    if let Some(dir) = default_export_folder() {
        dialog.set_initial_folder(Some(&gtk::gio::File::for_path(dir)));
    }

    let app = app.clone();
    dialog.save(Some(&w.window), None::<&gtk::gio::Cancellable>, move |result| {
        let Ok(file) = result else {
            // A cancelled dialog is not an error worth a dialog box.
            tracing::info!("export cancelled");
            return;
        };
        let format = if file
            .path()
            .and_then(|p| p.extension().map(|e| e == "txt"))
            .unwrap_or(false)
        {
            crate::export::Format::Text
        } else {
            crate::export::Format::Markdown
        };
        if let Err(e) = write_export(&note, format, file) {
            report_failure(&app, &format!("Export failed: {e}"));
        } else {
            tracing::info!(note = %note.id, format = ?format, "exported a note");
        }
    });
}

/// Default export folder, created on demand. `None` if it cannot be made, in
/// which case the dialog opens wherever the desktop last was.
pub(crate) fn default_export_folder() -> Option<std::path::PathBuf> {
    let dir = crate::export::default_dir();
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Write one note, plus its images when the destination is a folder.
pub(crate) fn write_export(
    note: &sn_store::model::Note,
    format: crate::export::Format,
    file: gtk::gio::File,
) -> std::io::Result<()> {
    let path = file
        .path()
        .ok_or_else(|| std::io::Error::other("the chosen destination has no path"))?;
    let bytes = crate::export::one(note, format);
    crate::export::write_file(&path, bytes.as_bytes())?;

    // A bundle travels as a folder: `<name>.md` beside an `images/`.
    if format == crate::export::Format::Bundle {
        let stem = path
            .file_stem()
            .map(|s| std::path::PathBuf::from(s).to_string_lossy().to_string())
            .unwrap_or_else(|| "note".into());
        let dir = path.with_file_name(&stem);
        std::fs::create_dir_all(&dir)?;
        let rebound = crate::export::rebind_images(&note.text);
        crate::export::write_file(&dir.join("note.md"), rebound.as_bytes())?;
        crate::export::copy_images(
            &sn_core::paths::note_images_dir(),
            &note.id,
            &dir,
        )?;
    }
    Ok(())
}

/// Ask for an image file, store it against the open note, and put the
/// reference where the caret is.
pub(crate) fn insert_image_into_open_note(app: &Rc<App>) {
    let Some(id) = app.open_note.borrow().clone() else {
        return;
    };
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };

    // `FileFilter` is in `gtk`, not `gio` — the dialog belongs to GTK even
    // though the file it returns is a GIO object. The `ListStore` is
    // parameterless in gio 0.20; its item type comes from the filter appended
    // to it.
    let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Images"));
    for pattern in ["*.png", "*.jpg", "*.jpeg", "*.gif", "*.webp", "*.bmp"] {
        filter.add_pattern(pattern);
    }
    filters.append(&filter);

    let dialog = gtk::FileDialog::new();
    dialog.set_title("Insert an image");
    dialog.set_filters(Some(&filters));

    let app = app.clone();
    let tv = w.editor_text.clone();
    dialog.open(Some(&w.window), None::<&gtk::gio::Cancellable>, move |result| {
        let Ok(file) = result else {
            return;
        };
        let Some(source) = file.path() else {
            report_failure(&app, "That file has no path.");
            return;
        };
        let bytes = match std::fs::read(&source) {
            Ok(b) => b,
            Err(e) => {
                report_failure(&app, &format!("Could not read that image: {e}"));
                return;
            }
        };
        let stored = match crate::images::store(
            &sn_core::paths::note_images_dir(),
            &id,
            &source,
            &bytes,
        ) {
            Ok(s) => s,
            Err(e) => {
                report_failure(&app, &format!("Could not store that image: {e}"));
                return;
            }
        };

        // Commit whatever was being typed first: the edit reads the buffer, so
        // a pending save must not be lost by the insert.
        commit_pending_edit(&app);
        let buffer = tv.buffer();
        let (text, sel) = crate::format::text_and_selection(&buffer);
        let alt = crate::format::selected_text(&text, sel);
        let edit = crate::format::insert_image(&text, sel, &stored.name, &alt);
        edit.apply(&buffer);
        tv.grab_focus();
        tracing::info!(note = %id, image = %stored.name, bytes = stored.bytes, "inserted an image");
    });
}