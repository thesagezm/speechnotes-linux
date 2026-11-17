//! Split out of reader.rs 2026-10-31 (pure code-motion).

use std::rc::Rc;
use gtk4::prelude::*;

use crate::app::App;
use super::*;

/// Ask for a book and import it, then open it. Every kind comes through
/// here: the chooser offers the formats by family, and the extension
/// decides the import path. A cancelled dialog is not an error.
pub fn pick_and_import(app: &Rc<App>) {
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    let dialog = gtk4::FileDialog::new();
    let epubs = gtk4::FileFilter::new();
    epubs.set_name(Some("EPUB books"));
    epubs.add_pattern("*.epub");
    let pdfs = gtk4::FileFilter::new();
    pdfs.set_name(Some("PDF documents"));
    pdfs.add_pattern("*.pdf");
    let decks = gtk4::FileFilter::new();
    decks.set_name(Some("Slide decks"));
    for pattern in ["*.pptx", "*.ppsx", "*.potx", "*.ppt", "*.odp"] {
        decks.add_pattern(pattern);
    }
    let audio = gtk4::FileFilter::new();
    audio.set_name(Some("Audiobooks"));
    for pattern in ["*.m4b", "*.m4a", "*.mp4", "*.opus", "*.ogg", "*.oga", "*.mp3"] {
        audio.add_pattern(pattern);
    }
    // The headless documents: each family its own filter, the same way the
    // extension table splits them, and every pattern also in "All books".
    let kindle = gtk4::FileFilter::new();
    kindle.set_name(Some("Kindle & Mobipocket"));
    for pattern in ["*.mobi", "*.azw", "*.azw3", "*.prc", "*.pdb"] {
        kindle.add_pattern(pattern);
    }
    let fb2 = gtk4::FileFilter::new();
    fb2.set_name(Some("FictionBook"));
    fb2.add_pattern("*.fb2");
    let rtf = gtk4::FileFilter::new();
    rtf.set_name(Some("RTF"));
    rtf.add_pattern("*.rtf");
    let html = gtk4::FileFilter::new();
    html.set_name(Some("HTML books"));
    for pattern in ["*.html", "*.htm", "*.htmlz"] {
        html.add_pattern(pattern);
    }
    let plain = gtk4::FileFilter::new();
    plain.set_name(Some("Plain text"));
    for pattern in ["*.txt", "*.text"] {
        plain.add_pattern(pattern);
    }
    let all = gtk4::FileFilter::new();
    all.set_name(Some("All books"));
    for pattern in [
        "*.epub",
        "*.pdf",
        "*.pptx",
        "*.ppsx",
        "*.potx",
        "*.ppt",
        "*.odp",
        "*.m4b",
        "*.m4a",
        "*.mp4",
        "*.opus",
        "*.ogg",
        "*.oga",
        "*.mp3",
        "*.mobi",
        "*.azw",
        "*.azw3",
        "*.prc",
        "*.pdb",
        "*.fb2",
        "*.rtf",
        "*.html",
        "*.htm",
        "*.htmlz",
        "*.txt",
        "*.text",
    ] {
        all.add_pattern(pattern);
    }
    let filters = gtk4::gio::ListStore::new::<gtk4::FileFilter>();
    filters.append(&all);
    filters.append(&epubs);
    filters.append(&pdfs);
    filters.append(&decks);
    filters.append(&audio);
    filters.append(&kindle);
    filters.append(&fb2);
    filters.append(&rtf);
    filters.append(&html);
    filters.append(&plain);
    dialog.set_filters(Some(&filters));

    let app = app.clone();
    dialog.open(
        Some(&w.window),
        None::<&gtk4::gio::Cancellable>,
        move |result| {
            let Ok(file) = result else {
                return; // cancelled
            };
            let Some(path) = file.path() else {
                return;
            };
            // The import runs on a worker: a PDF import parses the whole
            // file and rasterises a cover, which is seconds of frozen window
            // when it happens in the dialog's callback. The status line says
            // what is happening, and the shelf refreshes when it lands.
            set_status(&app, "Importing…");
            crate::tasks::spawn(move || {
                let extension = path
                    .extension()
                    .map(|e| e.to_string_lossy().to_ascii_lowercase())
                    .unwrap_or_default();
                // The format decides the import path: an EPUB goes through the
                // archive parser, an audiobook through the container reader,
                // and the headless documents (the Mobipocket/Kindle family,
                // FB2, RTF, HTML, plain text — the chooser's own filters
                // above) through the document parser.
                let imported = match extension.as_str() {
                    "epub" => sn_store::books::BooksStore::import(&path),
                    "pdf" => sn_store::books::BooksStore::import_pdf(&path),
                    "pptx" | "ppsx" | "potx" | "ppt" | "odp" => {
                        sn_store::books::BooksStore::import_slides(&path)
                    }
                    ext
                        if sn_doc::bookdoc::DocFamily::is_document_extension(ext) =>
                    {
                        sn_store::books::BooksStore::import_document(&path)
                    }
                    _ => sn_store::books::BooksStore::import_audiobook(&path),
                };
                crate::tasks::Msg::ImportDone {
                    result: imported
                        .map(|book| book.manifest)
                        .map_err(|e| e.to_string()),
                }
            });
        },
    );
}

/// The import finished: refresh the shelf and open what landed.
pub fn on_import_done(
    app: &Rc<App>,
    result: Result<sn_store::books::BookManifest, String>,
) {
    let manifest = match result {
        Ok(manifest) => manifest,
        Err(e) => {
            set_status(app, &format!("Import failed: {e}"));
            return;
        }
    };
    app.library_dirty.set(true);
    crate::window::refresh_sidebar(app);
    crate::window::refresh_list(app);
    // An EPUB opens on its first chapter with prose; an audiobook opens on
    // its first chapter. Passing 0 for an EPUB landed the reader on the
    // cover page of every book whose chapter 0 is front matter.
    let book = sn_store::books::BooksStore::from_manifest(manifest);
    let chapter = if book.is_audiobook() {
        0
    } else {
        book.first_speakable_chapter()
    };
    open_book(app, &book.manifest.id, chapter);
}

/// The startup repair sweep finished. It only matters if it changed
/// something: an unchanged library costs the sidebar nothing.
pub fn on_repair_done(app: &Rc<App>, repaired: Vec<String>) {
    if repaired.is_empty() {
        return;
    }
    for id in &repaired {
        tracing::info!(book = %id, "repaired a library manifest");
    }
    app.library_dirty.set(true);
    crate::window::refresh_sidebar(app);
    crate::window::refresh_list(app);
}

/// The startup repair sweep, on a worker.
///
/// It walks every manifest and rasterises a cover for any PDF that has
/// none — `pdftoppm` per book, seconds — and it used to run before the
/// first frame, so the app's start-up time was the library's size. It now
/// runs alongside the first window and only refreshes the shelf if it
/// actually repaired something.
pub fn start_repair_sweep() {
    crate::tasks::spawn(|| {
        let repaired = sn_store::books::BooksStore::repair_library()
            .unwrap_or_default();
        crate::tasks::Msg::RepairDone { repaired }
    });
}

