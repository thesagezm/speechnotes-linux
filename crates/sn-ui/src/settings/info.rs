//! Split out of settings.rs 2026-10-31 (pure code-motion).

use std::rc::Rc;

use gtk4 as gtk;
use gtk::prelude::*;

use crate::app::App;
use super::helpers::*;

// ---------------------------------------------------------------------------
// Shortcuts
// ---------------------------------------------------------------------------

pub(crate) fn shortcuts_page(page: &gtk::Box) {
    section(page, "Keyboard");
    for (keys, what) in SHORTCUTS {
        let line = gtk::Label::builder()
            .halign(gtk::Align::Start)
            .use_markup(true)
            .label(format!(
                // Pango markup: the key chord must be XML-escaped or a bare
                // `&` in a chord breaks the label's parse.
                "<tt>{}</tt>  —  {}",
                keys.replace('&', "&amp;"),
                what
            ))
            .build();
        page.append(&line);
    }
}

// ---------------------------------------------------------------------------
// About
// ---------------------------------------------------------------------------

pub(crate) fn about_page(app: &Rc<App>, page: &gtk::Box) {
    section(page, "About");
    let name = gtk::Label::builder()
        .label(format!("Speechnotes {}", env!("CARGO_PKG_VERSION")))
        .halign(gtk::Align::Start)
        .build();
    name.add_css_class("row-title");
    page.append(&name);

    let blurb = gtk::Label::builder()
        .label("A note is heard, not only read. Notes, books and audiobooks for Linux, speaking offline.")
        .halign(gtk::Align::Start)
        .wrap(true)
        .build();
    page.append(&blurb);

    let licence = gtk::Label::builder()
        .label("Licensed under the GNU General Public License v3 or later.")
        .halign(gtk::Align::Start)
        .wrap(true)
        .build();
    licence.add_css_class("row-meta");
    page.append(&licence);

    let logs = gtk::Button::with_label("Open the log file");
    {
        let app = app.clone();
        logs.connect_clicked(move |_| {
            let path = sn_core::paths::log_file();
            open_path(&app, &path);
        });
    }
    page.append(&logs);
}

pub(crate) const SHORTCUTS: [(&str, &str); 17] = [
    ("Ctrl+N", "New note"),
    ("Ctrl+Shift+N", "New notebook"),
    ("Ctrl+F", "Search"),
    ("Ctrl+1 / 2 / 3", "All notes / Unfiled / Bin"),
    ("Ctrl+B / I / `", "Bold / italic / code"),
    ("Ctrl+Alt+1", "Heading"),
    ("Ctrl+K", "Insert link"),
    ("Ctrl+E", "Focus the editor"),
    ("Ctrl+Shift+E", "Export this note"),
    ("Ctrl+Shift+I", "Insert an image"),
    ("Ctrl+Shift+P", "Pin / unpin"),
    ("Ctrl+Shift+S", "Favourite"),
    ("Ctrl+Shift+D", "Move to bin (asks first)"),
    ("Ctrl+Shift+V", "Toggle the rendered preview"),
    ("F5", "Play / pause"),
    ("F6", "Stop"),
    ("Ctrl+W", "Quit"),
];

