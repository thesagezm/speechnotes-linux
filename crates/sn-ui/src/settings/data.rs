//! Split out of settings.rs 2026-10-31 (pure code-motion).

use std::rc::Rc;

use gtk4 as gtk;
use gtk::prelude::*;

use crate::app::App;
use super::helpers::*;

// ---------------------------------------------------------------------------
// Data & storage
// ---------------------------------------------------------------------------

pub(crate) fn data_page(app: &Rc<App>, page: &gtk::Box) {
    section(page, "Library");
    let images = image_stats(app);
    let notes = app.notes.active().len();
    let binned = app.notes.binned_active().len();
    let books = sn_store::books::library_cached(&app.library, &app.library_dirty)
        .map(|b| b.len())
        .unwrap_or(0);
    let rows = [
        ("Notes", notes.to_string()),
        ("In the bin", binned.to_string()),
        ("Books", books.to_string()),
        ("Note images", format!("{} file(s) across {} note(s)", images.0, images.1)),
        ("Library size", human_size(dir_size(&sn_core::paths::books_dir()))),
        ("Models", human_size(dir_size(&sn_core::paths::models_dir()))),
        ("Cache", human_size(dir_size(&sn_core::paths::cache_dir()))),
    ];
    for (label, value) in rows {
        let r = row(label);
        let v = gtk::Label::new(Some(value.as_str()));
        v.add_css_class("row-meta");
        r.append(&v);
        page.append(&r);
    }

    section(page, "Files");
    let notes_button = gtk::Button::with_label("Open the notes file");
    {
        let app = app.clone();
        notes_button.connect_clicked(move |_| {
            let path = sn_core::paths::notes_file();
            open_path(&app, &path);
        });
    }
    page.append(&notes_button);

    let books_button = gtk::Button::with_label("Open the book library folder");
    {
        let app = app.clone();
        books_button.connect_clicked(move |_| {
            let path = sn_core::paths::books_dir();
            open_path(&app, &path);
        });
    }
    page.append(&books_button);

    let logs = gtk::Button::with_label("Open the log file");
    {
        let app = app.clone();
        logs.connect_clicked(move |_| {
            let path = sn_core::paths::log_file();
            open_path(&app, &path);
        });
    }
    page.append(&logs);

    // User-reported (2026-10-30): the store could import and
    // build JEX (Joplin export) archives — BookDrop's receiver and
    // `--import-note` both used it — but no button reached either half from
    // the app. The Data page is where the other file-level knobs live.
    section(page, "JEX (Joplin export)");
    let import = gtk::Button::with_label("Import a JEX archive…");
    {
        let app = app.clone();
        import.connect_clicked(move |_| pick_jex_import(&app));
    }
    page.append(&import);

    let export = gtk::Button::with_label("Export a notebook as JEX…");
    {
        let app = app.clone();
        export.connect_clicked(move |_| crate::dialogs::pick_jex_export(&app));
    }
    page.append(&export);
}

/// Pick a `.jex` file and import it into the library. The archive's notes and
/// notebooks merge in on name (Joplin's own rule); the worker thread keeps
/// the window alive while the tar is parsed.
//  authored for the JEX interchange section above,
// following `reader::pick_and_import`'s dialog + worker pattern.
pub(crate) fn pick_jex_import(app: &Rc<App>) {
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    let dialog = gtk::FileDialog::new();
    let jex = gtk::FileFilter::new();
    jex.set_name(Some("Joplin export (.jex)"));
    jex.add_pattern("*.jex");
    let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&jex);
    dialog.set_filters(Some(&filters));

    let app = app.clone();
    dialog.open(
        Some(&w.window),
        None::<&gtk::gio::Cancellable>,
        move |result| {
            let Ok(file) = result else {
                return; // cancelled
            };
            let Some(path) = file.path() else {
                return;
            };
            crate::reader::set_status(&app, "Importing JEX…");
            let notes = app.notes.clone();
            let notebooks = app.notebooks.clone();
            crate::tasks::spawn(move || {
                let result = crate::export::jex_import(&notes, &notebooks, &path);
                crate::tasks::Msg::JexImportDone { result }
            });
        },
    );
}

/// The save half of the JEX export: one save dialog, then the archive is
/// built and written on a worker.
//  authored for the JEX interchange section above.
pub(crate) fn save_jex_export(app: &Rc<App>, notebook_id: Option<String>, suggested: &str) {
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    let dialog = gtk::FileDialog::new();
    dialog.set_initial_name(Some(suggested));
    let app = app.clone();
    dialog.save(
        Some(&w.window),
        None::<&gtk::gio::Cancellable>,
        move |result| {
            let Ok(file) = result else {
                return; // cancelled
            };
            let Some(path) = file.path() else {
                return;
            };
            crate::reader::set_status(&app, "Building the JEX archive…");
            let notes = app.notes.clone();
            let notebooks = app.notebooks.clone();
            crate::tasks::spawn(move || {
                let bytes = crate::export::jex_archive(
                    &notes,
                    &notebooks,
                    notebook_id.as_deref(),
                );
                let result = crate::export::write_file(&path, &bytes)
                    .map(|_| {
                        format!(
                            "Exported {} to {}",
                            human_size(bytes.len() as u64),
                            path.display()
                        )
                    })
                    .map_err(|e| e.to_string());
                crate::tasks::Msg::JexExportDone { result }
            });
        },
    );
}

/// The JEX import's worker finished: refresh the list, tell the status line.
//  authored for the JEX interchange section above.
pub fn on_jex_import_done(app: &Rc<App>, result: Result<String, String>) {
    match result {
        Ok(summary) => {
            crate::window::refresh_all(app);
            crate::reader::set_status(app, &summary);
        }
        Err(e) => crate::reader::set_status(app, &format!("Import failed: {e}")),
    }
}

/// The JEX export's worker finished: the file is written (or not).
//  authored for the JEX interchange section above.
pub fn on_jex_export_done(app: &Rc<App>, result: Result<String, String>) {
    match result {
        Ok(summary) => crate::reader::set_status(app, &summary),
        Err(e) => crate::reader::set_status(app, &format!("Export failed: {e}")),
    }
}
