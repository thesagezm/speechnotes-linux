//! Split out of reader.rs 2026-10-31 (pure code-motion).

use std::rc::Rc;
use gtk4::glib;
use gtk4::prelude::*;

use crate::app::App;
use super::*;

/// Show the floating transport — fill-the-window mode only — and re-arm its
/// hide timer. Called on entering the mode and on every mouse motion over
/// the page: the controls are there when the reader reaches for them, gone
/// when the mouse rests.
pub fn float_poke(app: &Rc<App>) {
    if !app.focus_mode.get() || !page_visible(app) {
        return;
    }
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    w.reader.float.set_reveal_child(true);
    if let Some(old) = app.float_source.borrow_mut().take() {
        old.remove();
    }
    // The PowerPoint show (2026-11-12): a mirror show pins the pill —
    // it is the speaker's only control while the console is away — so
    // reaching for it must not start the hide timer against a control the
    // show cannot spare.
    if show_pinned(app) {
        return;
    }
    let app2 = app.clone();
    let source = glib::timeout_add_local(
        std::time::Duration::from_millis(FLOAT_HIDE_MS),
        move || {
            *app2.float_source.borrow_mut() = None;
            if let Some(w) = app2.widgets.borrow().clone() {
                w.reader.float.set_reveal_child(false);
            }
            glib::ControlFlow::Break
        },
    );
    *app.float_source.borrow_mut() = Some(source);
}

/// Hide the floating transport now: fill-the-window is off, so the pill has
/// no reason to exist.
pub fn float_hide(app: &Rc<App>) {
    if let Some(old) = app.float_source.borrow_mut().take() {
        old.remove();
    }
    if let Some(w) = app.widgets.borrow().clone() {
        w.reader.float.set_reveal_child(false);
    }
}

/// How far one Up/Down press travels: a wheel notch's worth of the
/// viewport, not most of a page — repeated presses reveal the next page
/// gradually, the way a mouse wheel does.
const WHEEL_STEP: f64 = 0.12;

/// Scroll whichever reader page is on screen by one wheel-notch step:
/// `direction` above 0 goes down, below 0 up. Returns `false` when the page
/// has nothing to scroll — the keyboard handler then leaves the key alone.
///
/// The scroller is picked from the stack's visible page, not the book's
/// format: a PDF shows either its pages or its extracted text, an EPUB the
/// book itself or the read-along text, and each is its own adjustment. The
/// EPUB's web page scrolls itself (a WebKit webview is no `Scrollable`, so
/// there is no adjustment to reach): it scrolls through the page's own
/// script, which reports where it stands, and the arrows turn the chapter
/// only when the page says it is at its edge.
pub fn scroll_reader(app: &Rc<App>, direction: f64) -> bool {
    let Some(w) = app.widgets.borrow().clone() else {
        return false;
    };
    let reader = &w.reader;
    let Some(page) = reader.stack.visible_child_name() else {
        return false;
    };
    let vadj = match page.as_str() {
        "pages" => reader.pdf.scroll.vadjustment(),
        "pdf-text" => reader.pdf_text_scroll.vadjustment(),
        "text" => reader.epub_text_scroll.vadjustment(),
        // A deck's slides are stepped, not scrolled: the keyboard handler
        // turns them into chapter moves before this is consulted.
        "slides" => return false,
        "slides-text" => reader.slides_text_scroll.vadjustment(),
        "engine" => {
            // The book view: a paged book steps a page, the same move
            // Left/Right make; a scrolled book scrolls itself, reporting
            // where it stands, and the arrows turn the section only when
            // the page says it is at its edge — the EPUB's protocol.
            if reader.book.flow() == "paged" {
                if direction < 0.0 {
                    reader.book.prev();
                } else {
                    reader.book.next();
                }
                return true;
            }
            let moved = match reader.book.scroll_state() {
                Some((y, max)) if max > 1.0 => {
                    if direction < 0.0 {
                        y > 0.5
                    } else {
                        y < max - 0.5
                    }
                }
                _ => true,
            };
            if moved {
                reader.book.scroll_by(direction);
            }
            return moved;
        }
        "book" => {
            // A paged book has columns, not a scroll: Up/Down step a
            // column, the same move Left/Right make.
            if reader.epub.appearance().flow == "paged" {
                reader.epub.turn_page(direction.signum() as i32);
                return true;
            }
            // The page scrolls itself, and reports where it stands, so the
            // arrows can tell "scroll me" from "at the edge — turn the
            // chapter". No report yet (the page is still laying out) errs
            // toward scrolling, the likelier ask.
            let moved = match reader.epub.scroll_state() {
                Some((y, max)) if max > 1.0 => {
                    if direction < 0.0 {
                        y > 0.5
                    } else {
                        y < max - 0.5
                    }
                }
                _ => true,
            };
            if moved {
                reader.epub.scroll_by(direction);
            }
            return moved;
        }
        _ => return false,
    };
    if vadj.page_size() <= 0.0 {
        return false;
    }
    // One wheel notch per press: repeated presses walk the next page in
    // gradually instead of throwing most of it past the eye at once.
    let value = vadj.value();
    let target = value + direction * WHEEL_STEP * vadj.page_size();
    let max = (vadj.upper() - vadj.page_size()).max(vadj.lower());
    vadj.set_value(target.clamp(vadj.lower(), max));
    // "Moved" is the honest answer: a page that fits the viewport and one
    // already at its end cannot travel, and the keyboard handler turns the
    // chapter instead — space at the foot of a page is how a reader asks
    // for the next one. Reporting true here (the old behaviour) ate the
    // key and the page never turned.
    (target - value).abs() > 0.5
}



/// The reader's status line.
pub fn set_status(app: &Rc<App>, text: &str) {
    if let Some(w) = app.widgets.borrow().as_ref() {
        w.reader.status.set_text(text);
    }
}

/// Show the reader's own appearance controls in the settings window.
///
/// The reader's toolbar carries the appearance button; pressing it opens the
/// same Book reader page the settings window has, so there is one place the
/// book's typography is set rather than two that can disagree.
pub fn show_reader_settings(app: &Rc<App>) {
    crate::settings::show_reader_page(app);
}

/// Fixed 2026-11-01: the reader page's own key controller, in
/// the **capture** phase.
///
/// The window's bubble-phase controller only sees the keys no focused widget
/// consumed — and on the reader page the focused widget is usually the
/// book's webview, which swallows every arrow before the window hears of
/// it. In fill-the-window the arrows (and F5/F11) did nothing until the
/// reader clicked a toolbar button to pull the focus off the page. A
/// capture-phase controller on the reader's own box sees each key *before*
/// the focused widget does, so the page's keys work whatever holds the
/// focus. Widgets that own their arrows — the search entry, the speed
/// spins, the read-along text views — are left alone, and so is a space on
/// a focused button (that activates it).
pub fn install_page_keys(app: &Rc<App>, page: &gtk4::Box) {
    use gtk4::glib::Propagation;
    let app = app.clone();
    let page_in = page.clone();
    let controller = gtk4::EventControllerKey::new();
    controller.set_propagation_phase(gtk4::PropagationPhase::Capture);
    controller.connect_key_pressed(move |_, keyval, _, state| {
        use gtk4::gdk::Key;
        let ctrl = state.contains(gtk4::gdk::ModifierType::CONTROL_MASK);
        let shift = state.contains(gtk4::gdk::ModifierType::SHIFT_MASK);
        if ctrl || shift {
            return Propagation::Proceed;
        }
        // Escape leaves fill-the-window before anything on the page can
        // claim the key — the capture phase sees it first, so a webview or
        // a focused page widget cannot swallow it. F11's inverse, on every
        // reader page (the note page's route is the window's controller;
        // the deck's webview posts the same exit when IT holds the focus).
        if keyval == Key::Escape && app.focus_mode.get() {
            crate::window::apply_focus_mode(&app, false);
            return Propagation::Stop;
        }
        // Widgets that keep their own arrows.
        let focused = page_in
            .root()
            .and_then(|r| r.downcast::<gtk4::Window>().ok())
            .and_then(|w| w.property::<Option<gtk4::Widget>>("focus-widget"));
        if let Some(fw) = focused {
            if fw.is::<gtk4::Entry>() || fw.is::<gtk4::SpinButton>() || fw.is::<gtk4::TextView>() {
                return Propagation::Proceed;
            }
            if keyval == Key::space && fw.is::<gtk4::Button>() {
                return Propagation::Proceed;
            }
        }
        // The transport and window keys — the same routes the window's
        // controller runs, taken before the webview can swallow them.
        match keyval {
            Key::F5 => {
                if !crate::abook::maybe_toggle(&app)
                    && !crate::speaksel::speak_from_selection(&app)
                {
                    crate::reader::speak_chapter(&app);
                }
                return Propagation::Stop;
            }
            Key::F6 => {
                if crate::abook::is_live(&app) {
                    crate::abook::close(&app);
                } else {
                    crate::player::stop_playback(&app);
                }
                return Propagation::Stop;
            }
            Key::F11 => {
                crate::window::apply_focus_mode(&app, !app.focus_mode.get());
                return Propagation::Stop;
            }
            _ => {}
        }
        // The arrows: the same map the window's controller carries.
        let deck = crate::reader::slides_open(&app);
        match keyval {
            Key::Left => {
                crate::reader::go_to_chapter(&app, -1);
                Propagation::Stop
            }
            Key::Right => {
                crate::reader::go_to_chapter(&app, 1);
                Propagation::Stop
            }
            Key::Up | Key::Page_Up => {
                if deck {
                    crate::reader::go_to_chapter(&app, -1);
                } else if !crate::reader::scroll_reader(&app, -1.0) {
                    // The page cannot travel (it fits the viewport, or the
                    // view is at its top): the reader is asking to go back.
                    crate::reader::go_to_chapter(&app, -1);
                }
                Propagation::Stop
            }
            Key::Down | Key::Page_Down | Key::space => {
                if deck {
                    crate::reader::go_to_chapter(&app, 1);
                } else if !crate::reader::scroll_reader(&app, 1.0) {
                    crate::reader::go_to_chapter(&app, 1);
                }
                Propagation::Stop
            }
            _ => Propagation::Proceed,
        }
    });
    page.add_controller(controller);
}
