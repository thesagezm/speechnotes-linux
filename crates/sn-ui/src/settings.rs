//! The settings window: one window, a page list on the left, one page on
//! the right — the shape the iOS app's Settings tab has, where every group
//! of settings is a subpage the user opens.
//!
//! The rule this module learned the hard way: **a control shows the stored
//! value once, at build; after that it writes through on change and never
//! re-reads mid-interaction.** A `SpinButton::set_value` in a refresh path
//! fires `value_changed` and the app eats its own settings (the GTK
//! SpinButton restart-loop lesson from Batch 8). So: build once, write
//! through, and only a full reopen re-syncs from the store.
//!
//! The pages, in the order they appear in the list:
//!
//! | Page | What lives there |
//! |---|---|
//! | Appearance | theme, accent, reading spacing, markdown rendering |
//! | Book reader | the book itself: theme, font, size, margins, flow |
//! | Speech | engine, voice, rate, audiobook speed, sleep timer |
//! | Models | the neural engines: install, progress, delete |
//! | BookDrop | the LAN receiver: toggle, access code, history |
//! | Data & storage | where things live, how big they are, the log |
//! | Shortcuts | the 17-command keyboard table |
//! | About | version, licence, the log file |

use std::rc::Rc;

use gtk4 as gtk;
use gtk::prelude::*;

use crate::app::App;

// Split submodules (pure code-motion 2026-10-31):
// the page builders live in `settings/`, glob re-exported so every existing
// `crate::settings::*` path resolves unchanged.
mod appearance;
mod bookdrop;
mod data;
mod helpers;
mod info;
mod models;
mod reader;
mod speech;

pub(crate) use appearance::*;
pub(crate) use bookdrop::*;
pub(crate) use data::*;
pub(crate) use models::*;
pub(crate) use reader::*;
pub(crate) use speech::*;
pub(crate) use helpers::*;
pub(crate) use info::*;

/// One settings page. The id is what the prefs store remembers, so it must
/// not change between releases: a renamed page would lose the user's place
/// (and a pref written by a newer app would land nowhere).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Appearance,
    Reader,
    Speech,
    Models,
    BookDrop,
    Data,
    Shortcuts,
    About,
}

impl Page {
    /// Every page, in list order.
    pub const ALL: [Page; 8] = [
        Page::Appearance,
        Page::Reader,
        Page::Speech,
        Page::Models,
        Page::BookDrop,
        Page::Data,
        Page::Shortcuts,
        Page::About,
    ];

    /// The stable id the prefs store keeps.
    pub fn id(self) -> &'static str {
        match self {
            Page::Appearance => "appearance",
            Page::Reader => "reader",
            Page::Speech => "speech",
            Page::Models => "models",
            Page::BookDrop => "bookdrop",
            Page::Data => "data",
            Page::Shortcuts => "shortcuts",
            Page::About => "about",
        }
    }

    /// The label in the page list.
    pub fn label(self) -> &'static str {
        match self {
            Page::Appearance => "Appearance",
            Page::Reader => "Book reader",
            Page::Speech => "Speech",
            Page::Models => "Models",
            Page::BookDrop => "BookDrop",
            Page::Data => "Data & storage",
            Page::Shortcuts => "Shortcuts",
            Page::About => "About",
        }
    }

    /// The icon beside the label. Every name here resolves from the system
    /// theme on Adwaita, Cosmic, Pop and breeze (the `icons` list is the
    /// contract); a name that does not resolve draws the image-missing
    /// placeholder, which is worse than no icon.
    pub fn icon(self) -> &'static str {
        match self {
            Page::Appearance => "preferences-desktop-font-symbolic",
            Page::Reader => "view-contents-symbolic",
            Page::Speech => "audio-speakers-symbolic",
            Page::Models => "folder-download-symbolic",
            Page::BookDrop => "network-transmit-receive-symbolic",
            Page::Data => "drive-multidisk-symbolic",
            Page::Shortcuts => "input-keyboard-symbolic",
            Page::About => "help-about-symbolic",
        }
    }

    /// The page a stored id names, or the first page when the id is one
    /// this build does not know (a newer app's page, or a typo).
    pub fn from_id(id: &str) -> Page {
        Page::ALL
            .iter()
            .copied()
            .find(|p| p.id() == id)
            .unwrap_or(Page::Appearance)
    }
}

/// Open the settings window on the Book reader page, from the reader's own
/// appearance button. The window is built if it does not exist yet, so the
/// reader's button always answers.
pub fn show_reader_page(app: &Rc<App>) {
    if find_settings_window().is_none() {
        show(app);
        return;
    }
    // The window is up: jump it to the reader page without rebuilding the
    // whole window, so the page the user was on is not lost.
    if let Some(state) = settings_state() {
        show_page(&state, Page::Reader);
    }
    if let Some(window) = find_settings_window() {
        window.present();
    }
}

// The live settings window's state, so a page can be shown in it from
// outside the window's own closure. A `thread_local!` rather than a
// `static`: the state is `Rc`, and GTK's own widgets are not `Sync`, so
// the only thread that may touch it is the UI thread that built it.
thread_local! {
    static SETTINGS_STATE: std::cell::RefCell<std::rc::Weak<std::cell::RefCell<SettingsState>>> =
        const { std::cell::RefCell::new(std::rc::Weak::new()) };
}

fn settings_state() -> Option<Rc<std::cell::RefCell<SettingsState>>> {
    SETTINGS_STATE.with(|cell| cell.borrow().upgrade())
}

/// Build and present the settings window. One at a time: a second click on
/// the gear raises the existing one rather than stacking a second dialog
/// over the same prefs.
pub fn show(app: &Rc<App>) {
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    if let Some(existing) = find_settings_window() {
        existing.present();
        return;
    }

    let prefs = app.prefs.get();
    // The page the user was last on, so reopening the window lands where
    // they left instead of jumping to the top every time.
    let page = Page::from_id(&prefs.settings_page);

    let window = gtk::Window::builder()
        .title("Settings")
        .transient_for(&w.window)
        .modal(false)
        .default_width(780)
        .default_height(620)
        .destroy_with_parent(true)
        .build();

    // A horizontal split: the page list on the left, the page on the right.
    // The list is a plain box of buttons rather than a `ListBox` so the
    // selected page can be marked with the same `active` class the app's
    // own sidebar uses.
    let outer = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .build();

    let list_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .css_classes(["settings-nav"])
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(8)
        .margin_end(4)
        .width_request(190)
        .build();

    let content = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .hexpand(true)
        .build();
    let page_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(16)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .build();
    content.set_child(Some(&page_box));

    outer.append(&list_box);
    outer.append(&content);
    window.set_child(Some(&outer));

    // The page currently shown. `gtk::Stack` would work, but the pages are
    // built once and swapped by removing the old one: a stack keeps every
    // page alive, and a page full of live widgets (the BookDrop poll, the
    // model progress bars) is cheaper to build on demand.
    let current = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(0)
        .build();
    content.set_child(Some(&current));

    // Build the page list and the page body. The two closures need each
    // other — a list button shows a page, and the page's own controls may
    // need to refresh the list — so the show function is written as one
    // small state machine with `Rc<RefCell<...>>`.
    let state: Rc<std::cell::RefCell<SettingsState>> =
        Rc::new(std::cell::RefCell::new(SettingsState {
            page,
            list_box: list_box.clone(),
            current: current.clone(),
            app: app.clone(),
            window: window.clone(),
            bookdrop_source: None,
        }));
    // The window's state is published, so the reader's appearance button can
    // drive this window's page list without holding the window (and
    // everything it owns) alive after it closes.
    SETTINGS_STATE.with(|cell| *cell.borrow_mut() = std::rc::Rc::downgrade(&state));
    {
        let state = state.clone();
        window.connect_destroy(move |_| {
            let _ = state;
            SETTINGS_STATE.with(|cell| *cell.borrow_mut() = std::rc::Weak::new());
        });
    }

    build_nav(&state);

    // The BookDrop page installs a poll timer; it is removed when the page
    // is left, so a closed window leaves no timer behind.
    {
        let state = state.clone();
        window.connect_destroy(move |_| {
            let mut s = state.borrow_mut();
            if let Some(source) = s.bookdrop_source.take() {
                source.remove();
            }
        });
    }

    show_page(&state, page);

    window.present();
}

/// The window's mutable state: which page is shown, and the widgets the
/// page list and the page body both need.
struct SettingsState {
    page: Page,
    list_box: gtk::Box,
    current: gtk::Box,
    app: Rc<App>,
    window: gtk::Window,
    /// The BookDrop page's poll timer, alive only while that page is up.
    bookdrop_source: Option<gtk::glib::SourceId>,
}

/// Build the page list: one flat button per page, the selected one marked.
fn build_nav(state: &Rc<std::cell::RefCell<SettingsState>>) {
    let s = state.borrow();
    //  this clear used to capture `next_sibling()` AFTER `remove()` —
    // a removed widget has no parent, so the property reads NULL and the
    // loop stopped after the first row. Every `show_page` then appended all
    // eight pages onto the leftovers and the nav grew by seven rows per
    // page switch (the "settings don't map correctly" report; measured in
    // the built app 2026-10-30). Drain until first_child() is empty, the
    // pattern imgcache/window already use.
    while let Some(c) = s.list_box.first_child() {
        s.list_box.remove(&c);
    }
    for page in Page::ALL {
        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .build();
        row.append(&gtk::Image::from_icon_name(page.icon()));
        row.append(&gtk::Label::builder().halign(gtk::Align::Start).label(page.label()).build());
        let button = gtk::Button::builder().child(&row).css_classes(["flat"]).build();
        button.set_hexpand(true);
        if page == s.page {
            button.add_css_class("active");
        }
        {
            let state = state.clone();
            button.connect_clicked(move |_| {
                let page = page;
                show_page(&state, page);
            });
        }
        s.list_box.append(&button);
    }
}

/// Show `page`: build it, put it in the content area, and remember it.
fn show_page(state: &Rc<std::cell::RefCell<SettingsState>>, page: Page) {
    let mut s = state.borrow_mut();
    // Leaving the BookDrop page stops its poll timer.
    if s.page != page {
        if let Some(source) = s.bookdrop_source.take() {
            source.remove();
        }
    }
    s.page = page;

    // Clear the content area. (Same removed-then-asked pattern the nav
    // clear had: it happened to work here because a page body is a single
    // child, but drain properly.)
    while let Some(c) = s.current.first_child() {
        s.current.remove(&c);
    }

    let app = s.app.clone();
    let window = s.window.clone();
    let target = s.current.clone();
    match page {
        Page::Appearance => appearance_page(&app, &target),
        Page::Reader => reader_page(&app, &target),
        Page::Speech => speech_page(&app, &target),
        Page::Models => models_page(&app, &target),
        Page::BookDrop => {
            let source = bookdrop_page(&app, &window, &target);
            s.bookdrop_source = source;
        }
        Page::Data => data_page(&app, &target),
        Page::Shortcuts => shortcuts_page(&target),
        Page::About => about_page(&app, &target),
    }

    // Remember the page for the next open, and rebuild the list so the
    // `active` marker follows.
    app.prefs.update(|p| p.settings_page = page.id().to_string());
    drop(s);
    build_nav(state);
}
