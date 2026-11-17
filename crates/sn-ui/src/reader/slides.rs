//! Split out of reader.rs 2026-10-31 (pure code-motion).

use std::rc::Rc;
use gtk4::prelude::*;

use crate::app::App;
use super::*;

pub(crate) fn is_slides_format(format: &str) -> bool {
    matches!(format, "pptx" | "ppsx" | "potx" | "ppt" | "odp")
}

/// Open a deck: every slide laid out as the deck laid it out, for the eyes;
/// the slide's speakable text for the voice.
///
/// The parse is memoised inside the Book, and the viewer takes an `Arc` of
/// it — the same parse the store's text layer reads, never mutated, with
/// OCR folded in by the store rather than by the deck.
pub(crate) fn open_slides(app: &Rc<App>, book: &sn_store::books::Book, index: usize, text: &str) {
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    let reader = &w.reader;
    // The zoom row serves a deck too (fit and zoom-in/out); the 100% button
    // is a PDF's idea and would lie about what fit means here.
    reader.pdf_tools.set_visible(true);
    reader.zoom_original.set_visible(false);
    reader.page_bar.set_visible(false);
    reader.epub_tools.set_visible(false);
    reader.title.set_text(&book.manifest.title);
    reader
        .status
        .set_text(&format!("slide {} / {}", index + 1, book.chapters()));

    let deck = match book.deck() {
        Ok(deck) => deck,
        Err(e) => {
            set_status(app, &format!("Could not open the deck: {e}"));
            return;
        }
    };
    //  the Present button is the deck's alone.
    reader.present.set_visible(true);
    reader.slides.set_theme(&app.prefs.get().reader_theme);
    reader.slides.set_fit(&app.prefs.get().slides_fit);
    {
        let app = app.clone();
        // A scroll inside the deck moves the reader's place: the same
        // follow-the-page work a PDF's page change does.
        reader.slides.on_slide(move |index| {
            on_slide_changed(&app, index);
        });
    }
    // PowerPoint show conventions (2026-11-07): the page's Esc ends
    // the show the same way the Present button's second click does — leave
    // fill-the-window, which ends the show (the audience screen and the
    // console) with it.
    {
        let app = app.clone();
        reader.slides.on_exit_present(move || {
            if app.focus_mode.get() {
                crate::window::apply_focus_mode(&app, false);
            }
        });
    }
    // The show's controls are the reader's navigation: the console's
    // buttons and the show windows' arrow keys take the same step the
    // deck's own keys do.
    {
        let app = app.clone();
        reader.slides.on_console_step(move |delta| {
            go_to_chapter(&app, delta);
        });
    }
    reader.slides.set_deck(deck);
    reader.slides.show_slide(index);
    // Focus mode *is* presentation mode for a deck; an open under focus
    // mode (a full-screen open from the shelf) starts presenting.
    reader.slides.set_present(
        app.focus_mode.get(),
        app.prefs.get().slides_click_advance,
    );
    reader.slides_text.buffer().set_text(text);
    reader.stack.set_visible_child_name("slides");
    //  a presenter window left open by the deck this one
    // replaces is already closed (set_deck does it); this fills the fresh
    // one, when presenting.
    presenter_refresh(app);
    //  the deck opens on its slides page, so the mode
    // toggle's state must say so — a pdf/epub session that ended in text
    // mode would otherwise make the first click flip the deck straight to
    // its slides again instead of to the slide text.
    app.reader.borrow_mut().pdf_text_mode = false;
    //  exchanged with the TOC button's icon (user report).
    reader.mode_toggle.set_icon_name("view-contents-symbolic");
    reader
        .mode_toggle
        .set_tooltip_text(Some("Read along with the slide text"));
}

/// The deck scrolled (or stepped) to another slide.
///
/// The mirror of `on_pdf_page_changed`: the reader's place, the status
/// line, the contents' active row all follow, and the read-along text
/// refills with the slide the voice is on — unless the voice is speaking,
/// in which case the view follows the voice and the text it is reading
/// stays.
pub(crate) fn on_slide_changed(app: &Rc<App>, index: usize) {
    let book_id = {
        let mut state = app.reader.borrow_mut();
        if state.chapter == index {
            return;
        }
        state.chapter = index;
        state.book_id.clone()
    };
    let Some(book_id) = book_id else {
        return;
    };
    let speaking = app.reader.borrow().speaking;
    let Ok(Some(book)) = current_book(app) else {
        return;
    };
    if !book.is_slides() {
        return;
    }
    // The slide's text layer costs no OCR (the cheap walk the plan chunks);
    // a picture's recognised text joins through `chapter_full`, which the
    // speech path asks for when it needs it.
    if let Ok(text) = book.chapter_text(index) {
        app.reader.borrow_mut().text = text.clone();
        if let Some(w) = app.widgets.borrow().clone() {
            w.reader.slides_text.buffer().set_text(&text);
        }
    }
    //  the presenter window follows the slide, the same
    // fill the read-along buffer just got.
    presenter_refresh(app);
    if let Some(w) = app.widgets.borrow().clone() {
        w.reader
            .status
            .set_text(&format!("slide {} / {}", index + 1, book.chapters()));
        mark_toc_row(app, index);
        refresh_controls(app);
    }
    if !speaking {
        // The debounced manifest write the PDF path uses; the slide is a
        // chapter, and the position records the same way.
        schedule_save_pdf_position(app, &book_id, index);
    }
}

/// Whether a deck is open in the reader.
pub fn slides_open(app: &Rc<App>) -> bool {
    let state = app.reader.borrow();
    is_slides_format(&state.format)
}

/// Whether the deck's slides page is the one on screen.
pub fn slides_view_visible(app: &Rc<App>) -> bool {
    if !slides_open(app) {
        return false;
    }
    let w = app.widgets.borrow().clone();
    w.and_then(|w| w.reader.stack.visible_child_name())
        .as_deref()
        == Some("slides")
}

/// Presentation mode follows the focus mode, for the open deck. Called by
/// `apply_focus_mode` on every flip; an open deck applies it itself too.
pub fn sync_slide_present(app: &Rc<App>) {
    if !slides_open(app) {
        return;
    }
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    //  leaving fill-the-window is leaving the presentation —
    // the show's windows (the audience screen, the console) go with it.
    if !app.focus_mode.get() {
        w.reader.slides.end_show();
        // The show just went: the pill's End button goes with it (the pill
        // itself is hidden by apply_focus_mode's own float_hide).
        sync_show_controls(app);
    } else {
        presenter_refresh(app);
    }
    // The flip also resized the page: the fit is re-decided at the new
    // size before the present state lands, so the first presented slide is
    // already the right scale.
    w.reader.slides.set_fit(&app.prefs.get().slides_fit);
    w.reader.slides.set_present(
        app.focus_mode.get(),
        app.prefs.get().slides_click_advance,
    );
}

///  the Present toolbar button, toggled. On: fill-the-window
/// (which *is* presentation, for a deck) plus the show's windows — with more
/// than one monitor the reader offers the choice between **Mirror** (the
/// slide fullscreen on both screens) and **Speaker view** (the slide
/// fullscreen on the other screen, the speaker's console on this one); with
/// one, the console floats over the deck. Off: everything goes.
pub(crate) fn toggle_present(app: &Rc<App>) {
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    if w.reader.slides.show_running() {
        // sync_slide_present (via apply_focus_mode) ends the show.
        crate::window::apply_focus_mode(app, false);
        return;
    }
    crate::window::apply_focus_mode(app, true);
    let monitors = gtk4::gdk::Display::default().map(|d| d.monitors());
    let n_monitors = monitors.as_ref().map(|m| m.n_items()).unwrap_or(1);
    if n_monitors <= 1 {
        // One screen: the console floats over the deck — there is no
        // audience screen to send the slide to.
        w.reader.slides.start_show(None, None);
        presenter_refresh(app);
        return;
    }
    // The drive's seam (`SN_PRESENT_MODE=speaker|mirror`): the show starts
    // in that way without the dialog. A test hook, not a setting — the
    // reader always sees the question on a real launch. (The AlertDialog's
    // buttons are not in the a11y tree on this GTK build, so the drive
    // could not answer it by press; measured 2026-11-12.)
    match std::env::var("SN_PRESENT_MODE").as_deref() {
        Ok("speaker") => {
            start_speaker(app);
            return;
        }
        Ok("mirror") => {
            start_mirror(app);
            return;
        }
        _ => {}
    }
    // Two screens: ask once, per presentation, which way to go. The
    // button index is the contract: 0 = Mirror, 1 = Speaker view. GTK 4.10
    // deprecated MessageDialog; this is the AlertDialog of the same words.
    let dialog = gtk4::AlertDialog::builder()
        .message("Present on two screens — how?")
        .detail(
            "Mirror shows the slide fullscreen on both screens. Speaker \
             view puts the slide fullscreen on the other screen and the \
             speaker's console — this slide, the next and the notes — on \
             this one.",
        )
        .modal(true)
        .build();
    dialog.set_buttons(&["Mirror", "Speaker view"]);
    // Enter picks Mirror (the no-op); dismissing the dialog returns the
    // cancel index (-1), which keeps the fill mode.
    dialog.set_default_button(0);
    let app = app.clone();
    dialog.choose(Some(&w.window), None::<&gtk4::gio::Cancellable>, move |result| {
        match result {
            Ok(0) => start_mirror(&app),
            Ok(1) => start_speaker(&app),
            _ => {} // dismissed: the fill mode stays, no show.
        }
    });
}

/// The **Mirror** show: the slide fullscreen on BOTH screens — the audience
/// window takes the monitor the main window is not on, by geometry (a
/// projector's port and position are its own).
fn start_mirror(app: &Rc<App>) {
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    if let Some(monitor) = other_monitor(&w.window) {
        w.reader.slides.start_show(Some(&monitor), None);
        presenter_refresh(app);
    }
}

/// The **Speaker view** show: the console fullscreens on THIS screen (the
/// one the main window sits on) and the slide fullscreens on the other —
/// PowerPoint's split, whichever way round the screens are plugged in.
fn start_speaker(app: &Rc<App>) {
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    let this_screen = window_monitor(&w.window);
    let other = other_monitor(&w.window);
    w.reader.slides.start_show(other.as_ref(), this_screen.as_ref());
    presenter_refresh(app);
}

/// The monitor a window sits on, by its surface — the ground truth of
/// "this screen", whatever the monitors' list order is.
fn window_monitor(window: &impl IsA<gtk4::Window>) -> Option<gtk4::gdk::Monitor> {
    let display = gtk4::gdk::Display::default()?;
    let surface = window.upcast_ref::<gtk4::Window>().surface()?;
    display.monitor_at_surface(&surface)
}

/// The monitor a window is NOT on: the show's audience screen. Picked by
/// geometry and model, never by list index — a projector plugged in
/// anywhere, at any scale, is still "the other screen".
fn other_monitor(window: &impl IsA<gtk4::Window>) -> Option<gtk4::gdk::Monitor> {
    let display = gtk4::gdk::Display::default()?;
    let here = window_monitor(window)?;
    let monitors = display.monitors();
    for i in 0..monitors.n_items() {
        let Some(m) = monitors.item(i).and_downcast::<gtk4::gdk::Monitor>() else {
            continue;
        };
        if !same_monitor(&m, &here) {
            return Some(m);
        }
    }
    None
}

/// gdk::Monitor carries no identity comparison in the bindings; geometry
/// and model together say "the same screen" well enough for the two-screen
/// decision (two monitors never share both on a sane desktop).
fn same_monitor(a: &gtk4::gdk::Monitor, b: &gtk4::gdk::Monitor) -> bool {
    a.geometry() == b.geometry() && a.model() == b.model()
}

/// Whether the floating transport is the running show's only control: a
/// show without its console — **mirror** mode, or the moment after the
/// console's own window was closed while the show continues. Reader chrome
/// (`float_poke`, `sync_show_controls`) reads this to pin the pill and show
/// its End button, so a speaker without a keyboard still has the show.
pub(crate) fn show_pinned(app: &Rc<App>) -> bool {
    let Some(w) = app.widgets.borrow().clone() else {
        return false;
    };
    w.reader.slides.show_running() && !w.reader.slides.console_open()
}

/// Keep the floating transport in step with the show. Called on every show
/// start and slide change (`presenter_refresh`), on the show's end
/// (`sync_slide_present`) and wherever a deck close ends the show. A mirror
/// show pins the pill — revealed for the whole show, hide timer cancelled —
/// and shows its End button; with the console up the pill keeps its
/// motion-revealed behavior and the End button hides (the console owns the
/// controls). The pin is also the drive's control: a pinned pill is in the
/// a11y tree with named buttons (`press 'Next chapter'`, `press 'End show'`)
/// and a live slide readout (`slide 16 / 100`).
pub(crate) fn sync_show_controls(app: &Rc<App>) {
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    let pinned = show_pinned(app);
    w.reader.float_end.set_visible(pinned);
    if pinned {
        // Pin: cancel a pending motion hide; the pill stays up for the
        // show. (`float_poke` does not re-arm the timer while pinned.)
        if let Some(old) = app.float_source.borrow_mut().take() {
            old.remove();
        }
        w.reader.float.set_reveal_child(true);
    }
}

///  fill the show from the reader's state — the audience
/// screen and the console's previews follow the deck's place, and the
/// console's readouts (the counter, the current slide's speaker text — the
/// same text layer the voice reads, taken straight from the book: the
/// read-along buffer mirrors it, but the voice may be speaking and the
/// buffer may be scrolled elsewhere; the book is the authority). A no-op
/// while no show runs.
pub(crate) fn presenter_refresh(app: &Rc<App>) {
    // The floating transport follows the show before the early return: a
    // mirror show pins it, a show's end unpins it — both on this path,
    // which every show start and slide change goes through.
    sync_show_controls(app);
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    if !w.reader.slides.show_running() {
        return;
    }
    let index = app.reader.borrow().chapter;
    w.reader.slides.present_follow(index);
    if !w.reader.slides.console_open() {
        return;
    }
    let Ok(Some(book)) = current_book(app) else {
        return;
    };
    //  PowerPoint's presenter reads "Slide 3 of 20".
    let counter = format!("Slide {} of {}", index + 1, book.chapters());
    let now_text = book.chapter_text(index).unwrap_or_default();
    w.reader.slides.presenter_update(&counter, now_text.trim());
}

/// The deck's own read-along prefs landed: re-apply the fit and the
/// presentation state to the open deck. Called by the settings page.
pub fn apply_slide_prefs(app: &Rc<App>) {
    if !slides_open(app) {
        return;
    }
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    let prefs = app.prefs.get();
    w.reader.slides.set_fit(&prefs.slides_fit);
    w.reader.slides.set_present(app.focus_mode.get(), prefs.slides_click_advance);
}

/// The zoom family, dispatched by the open format: a PDF zooms its rendered
/// page; a deck steps a manual zoom (out of any fit mode).
pub(crate) fn zoom_step(app: &Rc<App>, delta: i32) {
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    if app.reader.borrow().format == "pdf" {
        if delta > 0 {
            w.reader.pdf.zoom_in();
        } else {
            w.reader.pdf.zoom_out();
        }
    } else if is_slides_format(&app.reader.borrow().format) {
        w.reader.slides.zoom_step(delta);
    }
}

/// The fit button, dispatched the same way. For a deck it cycles the
/// stored fit — fill the width, or the whole slide — so the next open
/// remembers the choice.
pub(crate) fn zoom_fit_cycle(app: &Rc<App>) {
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    if app.reader.borrow().format == "pdf" {
        w.reader.pdf.zoom_fit();
    } else if is_slides_format(&app.reader.borrow().format) {
        let next = if app.prefs.get().slides_fit == "page" {
            "width"
        } else {
            "page"
        };
        app.prefs.update(|p| p.slides_fit = next.to_string());
        w.reader.slides.set_fit(next);
    }
}

