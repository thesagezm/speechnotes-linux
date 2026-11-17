//! The reader page's keyboard focus (, fix session 2026-11-02).
//!
//! GTK4 delivers a key event along the **focused widget's** ancestor
//! chain: the window's own key controller only sees a key that the
//! focused widget (and its ancestors) did not consume, and a window with
//! *no* focused widget delivers no key event at all. The reader's page was
//! such a window — nothing on the PDF pages view is focusable, and the
//! page's capture-phase controller (`reader::chrome::install_page_keys`)
//! could therefore never fire. That was the measured state of the live app
//! (the accessibility tree had no focused node at all) and it is exactly
//! the user's report: in fill-the-window the arrows did nothing until an
//! on-screen button was clicked, because that click was the first thing to
//! give the window a focus widget.
//!
//! The fix is one focusable widget — the reader page's own box — plus a
//! watchdog that takes the focus back when it drops.

use std::rc::Rc;

use gtk4::glib;
use gtk4::prelude::*;
use gtk4 as gtk;

use crate::app::App;

/// The window's focused widget, if it has one.
fn focused_widget(window: &gtk::ApplicationWindow) -> Option<gtk::Widget> {
    window.property::<Option<gtk::Widget>>("focus-widget")
}

/// A widget that owns its own arrow keys: the search entry, the speed
/// spins, the read-along text views. The reader's key controller makes the
/// same exception (`reader::chrome::install_page_keys`), so the focus and
/// the keys agree on who owns the keyboard.
fn keeps_own_keys(widget: &gtk::Widget) -> bool {
    widget.is::<gtk::Entry>()
        || widget.is::<gtk::SpinButton>()
        || widget.is::<gtk::TextView>()
}

/// Put the keyboard focus on the reader page's own box.
///
/// Called when the reader page is shown and when fill-the-window is
/// entered or left. The grab is deferred to an idle callback: the page has
/// to be on screen before it can hold the focus, and a grab from inside a
/// focus-change handler re-enters GTK's focus machinery.
pub fn grab_reader_focus(app: &Rc<App>) {
    focus_page(app, false);
}

/// Take the focus back **only if it is vacant**.
///
/// This is the watchdog's variant. A focus that appeared in the meantime
/// was put there by the reader, and the reader wins: the watchdog exists
/// to fill a hole, not to take a widget's place.
fn grab_vacant_focus(app: &Rc<App>) {
    focus_page(app, true);
}

/// The one grab, with the two policies it needs.
fn focus_page(app: &Rc<App>, only_if_vacant: bool) {
    let app = app.clone();
    glib::idle_add_local_once(move || {
        let Some(w) = app.widgets.borrow().clone() else {
            return;
        };
        // Only the reader page wants the keyboard: on the note page the
        // editor's own caret does.
        if !crate::reader::page_visible(&app) {
            return;
        }
        if let Some(fw) = focused_widget(&w.window) {
            // A widget *inside* the page that owns its keys keeps them —
            // the read-along text view the reader clicked into. A widget
            // outside the page does not: the page is what is on screen,
            // and the reader's arrows are the page's business.
            if keeps_own_keys(&fw) && w.reader.box_.is_ancestor(&fw) {
                return;
            }
            if only_if_vacant {
                return;
            }
        }
        w.reader.box_.grab_focus();
    });
}

/// Keep a focus widget alive while the reader page is on screen.
///
/// The focus drops whenever the widget holding it is hidden or replaced —
/// a toolbar row that fill-the-window hides, a button a page turn replaces
/// — and a window without a focused widget hears no key events at all.
/// This watches the window's `focus-widget` and puts the focus back when
/// it goes, so the reader's arrows keep working without a click first.
///
/// The handler only ever *adds* a focus widget; it never moves one. A
/// widget that holds the focus — the search entry, the read-along text
/// view, a toolbar button — keeps it, and the reader's capture-phase
/// controller (or the window's bubble-phase one, for the toolbar) answers
/// the keys as it always did.
///
/// The window's own activation is watched too: GTK can drop the focus
/// while the window is in the background, and the app must not reach for
/// the keyboard from there — it takes the focus back when the reader comes
/// *back* to the window instead.
pub fn install_focus_watchdog(window: &gtk::ApplicationWindow, app: &Rc<App>) {
    {
        let app = app.clone();
        window.connect_focus_widget_notify(move |w| {
            if w.property::<Option<gtk::Widget>>("focus-widget").is_some() {
                return;
            }
            if !w.is_active() {
                return;
            }
            if !crate::reader::page_visible(&app) {
                return;
            }
            grab_vacant_focus(&app);
        });
    }
    {
        let app = app.clone();
        window.connect_is_active_notify(move |w| {
            if !w.is_active() {
                return;
            }
            if w.property::<Option<gtk::Widget>>("focus-widget").is_some() {
                return;
            }
            if !crate::reader::page_visible(&app) {
                return;
            }
            grab_vacant_focus(&app);
        });
    }
}
