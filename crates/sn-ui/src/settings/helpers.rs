//! Split out of settings.rs 2026-10-31 (pure code-motion).

use std::rc::Rc;

use gtk4 as gtk;
use gtk::prelude::*;

use crate::app::App;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Raise the existing settings window if one is already open.
pub(crate) fn find_settings_window() -> Option<gtk::Window> {
    gtk::Window::list_toplevels()
        .iter()
        .filter_map(|w| w.downcast_ref::<gtk::Window>().cloned())
        .find(|w| w.title().as_deref() == Some("Settings"))
}

/// A section heading.
pub(crate) fn section(parent: &gtk::Box, title: &str) {
    let label = gtk::Label::builder()
        .label(title.to_uppercase())
        .halign(gtk::Align::Start)
        .css_classes(["settings-section"])
        .build();
    parent.append(&label);
}

/// A labelled row container.
pub(crate) fn row(label: &str) -> gtk::Box {
    let box_ = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    box_.append(&gtk::Label::new(Some(label)));
    box_
}

/// A check-button row; returns the container and the check itself.
pub(crate) fn toggle_row(label: &str, value: bool) -> (gtk::Box, gtk::CheckButton) {
    let check = gtk::CheckButton::with_label(label);
    check.set_active(value);
    let box_ = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .build();
    box_.append(&check);
    (box_, check)
}

/// Wire a check through to a prefs field.
pub(crate) fn wire_toggle(
    app: &Rc<App>,
    check: gtk::CheckButton,
    set: impl Fn(&mut sn_store::Prefs, bool) + 'static,
) {
    let app = app.clone();
    check.connect_toggled(move |c| {
        let value = c.is_active();
        app.prefs.update(|p| set(p, value));
    });
}

/// Wire a check through to a prefs field, with the book re-dressed after it.
///
/// A toggle on the Book reader page that only stored its value would look
/// broken: the book on screen would not move until it was closed and
/// reopened. Every one of these re-applies the appearance.
pub(crate) fn wire_reader_toggle(
    app: &Rc<App>,
    check: gtk::CheckButton,
    set: impl Fn(&mut sn_store::Prefs, bool) + 'static,
) {
    let app = app.clone();
    check.connect_toggled(move |c| {
        let value = c.is_active();
        app.prefs.update(|p| set(p, value));
        crate::reader::apply_epub_appearance(&app);
    });
}

/// (files, notes-with-images) for the data pane.
pub(crate) fn image_stats(app: &Rc<App>) -> (usize, usize) {
    let root = sn_core::paths::note_images_dir();
    let mut files = 0usize;
    let mut notes = 0usize;
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten() {
            let count = std::fs::read_dir(entry.path())
                .map(|d| d.flatten().count())
                .unwrap_or(0);
            if count > 0 {
                notes += 1;
            }
            files += count;
        }
    }
    let _ = app;
    (files, notes)
}

/// The total size of a directory tree, in bytes. One pass; the libraries
/// are small enough that a walk is cheaper than remembering sizes.
pub(crate) fn dir_size(dir: &std::path::Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            total += dir_size(&entry.path());
        } else {
            total += meta.len();
        }
    }
    total
}

/// A size in the units a person reads: "1.2 GB", "840 MB", "12 KB".
pub(crate) fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.0} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

/// Best-effort "show this file": xdg-open. A failure is logged, not a
/// dialog — the button's job is to try, and the log is where to look.
pub(crate) fn open_path(app: &Rc<App>, path: &std::path::Path) {
    if !path.exists() {
        crate::window::report_failure(app, "That file does not exist yet.");
        return;
    }
    let target = path.to_path_buf();
    std::thread::spawn(move || {
        let _ = std::process::Command::new("xdg-open")
            .arg(&target)
            .spawn();
    });
}

/// The shortcut table, as the window's controller implements it. A row here
/// that the controller does not honour is a doc bug, and vice versa — the
/// two live in one place each, and the reviewer reads both.
/// The display handle the swatch providers need. Panics are impossible in
/// practice: the settings dialog only opens from a live window.
pub(crate) fn gdk4_display() -> gtk::gdk::Display {
    gtk::gdk::Display::default()
        .expect("a default display exists while the app is running")
}
