//! The icons Speechnotes ships that the system themes do not have.
//!
//! Eight of the toolbar's icon names are Adwaita-invented and exist in no other
//! theme — `view-preview-symbolic`, `view-sort-alphabetical-symbolic`,
//! `format-text-heading-symbolic`, `format-text-bulleted-list-symbolic`,
//! `format-text-quote-symbolic`, `format-text-backslash-symbolic`,
//! `image-insert-symbolic` and `view-contents-symbolic`. On a GNOME box they
//! render; on this one (Cosmic) they fell back to the "image-missing"
//! placeholder, which is what the broken toolbar buttons were.
//!
//! The fix is to ship them. They live in `assets/icons-src` as source SVG
//! and are compiled into a GResource at build time, so there is no second copy
//! to install and no path to get wrong. [`register`] puts them on the icon
//! theme's search path before the first window is built.
//!
//! The theme file declares `Inherits=Pop,Adwaita,hicolor`, so registering it
//! costs nothing: every icon the system already had still resolves, and the
//! eight that were missing now resolve from inside the binary. Verified by
//! looking up each name through `GtkIconTheme::has_icon` with a live display —
//! `view-preview-symbolic` and friends came back true, and Adwaita's
//! `document-new-symbolic` kept resolving from `/usr/share/icons/Cosmic`.

use gtk4 as gtk;

/// The resource path the icon theme is mounted at.
///
/// This is the same prefix the manifest uses, and it must match: the icons are
/// registered at `<ICON_RESOURCE_ROOT>/` on the theme's search path, so the
/// manifest's `<gresource prefix>` ends at the directory that contains
/// `index.theme`.
pub const ICON_RESOURCE_ROOT: &str = "/io/github/thesagezm/Speechnotes/icons";

/// The application icon's name — the rounded-corner mark the iOS and Android
/// launchers show, shipped as fixed-size PNGs under the same resource as the
/// symbolic toolbar icons. `Window::set_default_icon_name` points every
/// window and the shell's taskbar entry at this name; the launcher itself
/// (the desktop entry) resolves the same name from the installed hicolor
/// copies, which live outside the binary.
pub const APP_ICON: &str = "speechnotes";

/// Mount the shipped icons so `GtkIconTheme` can find them.
///
/// Safe to call more than once; `Gio::resources_register` is idempotent and the
/// search path is a set. Call it from `startup`, before the first window: an
/// icon the theme has already resolved as missing stays missing until the theme
/// is rescanned, and a widget that drew the placeholder keeps its paintable.
pub fn register() {
    let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/icons.gresource"));
    let data = gtk::glib::Bytes::from_static(bytes);
    let resource = gtk::gio::Resource::from_data(&data)
        .expect("the bundled icon resource is a valid GResource");
    gtk::gio::resources_register(&resource);

    if let Some(display) = gtk::gdk::Display::default() {
        gtk::IconTheme::for_display(&display).add_resource_path(ICON_RESOURCE_ROOT);
    }
}

/// Every icon name the app asks for, shipped or inherited.
///
/// The reason this list is written down is that it is checkable: a test that
/// walks it catches the next `format-text-foo-symbolic` that exists only in
/// Adwaita before it becomes a blank button on someone else's desktop. A name
/// that resolves on this box today may not resolve here in three years, which
/// is exactly when nobody will remember what it looked like.
pub const SHIPPED: &[&str] = &[
    // Not an Adwaita-invented name like the rest: `audiobook-symbolic` is
    // simply absent from every theme on this box (Cosmic, Pop, Adwaita,
    // breeze), so the Library row and the import button drew the
    // image-missing placeholder. Shipped like the others.
    "audiobook-symbolic",
    "view-preview-symbolic",
    // The reader's contents button. Adwaita has no table-of-contents icon
    // under any name, and the names that come close — `view-list-ordered`,
    // `view-list-compact` — exist in Adwaita alone, so a Cosmic box would
    // draw the image-missing placeholder. Shipped: the contents button and
    // the reader-mode toggle must not wear the same face, and the toggle
    // already wears the system's list icon.
    "view-contents-symbolic",
    "view-sort-alphabetical-symbolic",
    "format-text-heading-symbolic",
    "format-text-bulleted-list-symbolic",
    "format-text-quote-symbolic",
    "format-text-backslash-symbolic",
    "image-insert-symbolic",
];

/// Names the app relies on that come from the system theme.
///
/// Kept alongside `SHIPPED` so a test can assert that shipping our seven did
/// not shadow the system's — the theme file's `Inherits` is what guarantees
/// that, and this is the list that would notice if it stopped doing so.
pub const INHERITED: &[&str] = &[
    "document-new-symbolic",
    // The audiobook transport. Adwaita, Pop and Cosmic all carry these; the
    // list exists so a theme change that drops one is caught by the check.
    "go-next-symbolic",
    "go-previous-symbolic",
    "media-playback-pause-symbolic",
    "media-playback-start-symbolic",
    "media-playback-stop-symbolic",
    "media-skip-backward-symbolic",
    // The audiobook page's next-chapter button. Named for the seek-forward
    // family, which Adwaita carries and the list here would notice losing.
    "media-seek-forward-symbolic",
    "folder-new-symbolic",
    "document-save-symbolic",
    "folder-symbolic",
    "folder-documents-symbolic",
    "view-list-symbolic",
    "view-grid-symbolic",
    "view-pin-symbolic",
    "starred-symbolic",
    "user-trash-symbolic",
    "insert-link-symbolic",
    "format-text-bold-symbolic",
    "format-text-italic-symbolic",
    "format-text-strikethrough-symbolic",
    "view-sort-ascending-symbolic",
    "view-sort-descending-symbolic",
    // Settings sidebar pages (added with the settings restructure; verified
    // against Adwaita, Cosmic, Pop and breeze on this box, same as the rest).
    "preferences-desktop-font-symbolic",
    "audio-speakers-symbolic",
    "folder-download-symbolic",
    "network-transmit-receive-symbolic",
    "drive-multidisk-symbolic",
    "input-keyboard-symbolic",
    "help-about-symbolic",
    // The PDF viewer's own controls. Every one of these is carried by
    // Adwaita, Pop and Cosmic on this box; the list exists so a theme
    // change that drops one is caught by the check rather than by a blank
    // button.
    "view-paged-symbolic",
    "zoom-out-symbolic",
    "zoom-fit-best-symbolic",
    "zoom-in-symbolic",
    "zoom-original-symbolic",
    // The sidebar's settings button. An emblem rather than an action, but the
    // icon theme resolves it the same way.
    "emblem-system-symbolic",
    // Expand-to-window. The toggle wears the fullscreen pair: expand wears
    // `view-fullscreen-symbolic`, restored wears `view-restore-symbolic`.
    // Standard names — carried by Cosmic, Pop, Adwaita and breeze on this
    // box, so they inherit rather than ship.
    "view-fullscreen-symbolic",
    "view-restore-symbolic",
];

/// True when every icon the app asks for resolves on the current display.
///
/// Needs a display, so it is called from the running app and from
/// `examples/check_icons`, never from `--selftest`.
pub fn all_resolve() -> bool {
    let Some(display) = gtk::gdk::Display::default() else {
        return false;
    };
    let theme = gtk::IconTheme::for_display(&display);
    SHIPPED
        .iter()
        .chain(INHERITED.iter())
        .all(|name| theme.has_icon(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped names are what the resource contains; a typo here ships an
    /// icon nobody can look up.
    #[test]
    fn shipped_names_match_the_resource() {
        let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/icons.gresource"));
        let data = gtk::glib::Bytes::from_static(bytes);
        let resource =
            gtk::gio::Resource::from_data(&data).expect("resource");
        for name in SHIPPED {
            let path = format!("{ICON_RESOURCE_ROOT}/scalable/actions/{name}.svg");
            let found = resource
                .lookup_data(&path, gtk::gio::ResourceLookupFlags::NONE)
                .is_ok();
            assert!(found, "{path} is not in the bundled resource");
        }
    }

    /// Nothing may appear in both lists: a shipped icon that is also inherited
    /// means the theme file's directory and the resource's disagree.
    #[test]
    fn no_name_is_both_shipped_and_inherited() {
        for name in SHIPPED {
            assert!(
                !INHERITED.contains(name),
                "{name} resolves from the system theme; do not ship it"
            );
        }
    }

    /// The application icon rides in the same resource, under the sized
    /// `apps` directories the theme file declares — one PNG per size, so a
    /// lookup at any size finds a real file rather than scaling the 1024
    /// master on the fly.
    #[test]
    fn the_app_icon_is_in_the_resource() {
        let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/icons.gresource"));
        let data = gtk::glib::Bytes::from_static(bytes);
        let resource =
            gtk::gio::Resource::from_data(&data).expect("resource");
        for size in ["128x128", "256x256", "512x512"] {
            let path = format!("{ICON_RESOURCE_ROOT}/{size}/apps/{APP_ICON}.png");
            assert!(
                resource
                    .lookup_data(&path, gtk::gio::ResourceLookupFlags::NONE)
                    .is_ok(),
                "{path} is not in the bundled resource"
            );
        }
    }
}
