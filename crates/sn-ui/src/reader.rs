//! The book reader: the shelf, the chapter pane, the TOC and per-chapter
//! speech.
//!
//! The design is the note editor's, one level up: the shelf is a third
//! `View`, the chapter pane is a fourth page in the editor stack, and the
//! speech path is the same `SessionCore` the mini-player drives — a chapter
//! is just a `text` with a byte offset, exactly like a note. What is new:
//!
//! * **Position is (chapter, fraction).** The bookmark store already keys
//!   books by `book:<id>:<chapter>` (the iOS contract); the fraction through
//!   the chapter comes from the session frontier divided by the chapter's
//!   byte length, so a bookmark written mid-chapter resumes mid-chapter.
//! * **The TOC drives position.** A TOC row names a spine index; selecting
//!   one sets `position.chapterIndex` and saves the manifest, so the shelf
//!   can show progress and the next launch resumes there.
//! * **A chapter's text is extracted on demand** from the archive — the
//!   manifest carries the spine, not the text, so the shelf never opens a
//!   book and the reader never re-parses one it has already read.
//! * **Search is over the chapters, not the archive.** The in-book search
//!   walks the spine and reports chapter hits with their byte offsets, so a
//!   result can jump straight there.

use std::rc::Rc;

use gtk4::glib;
use gtk4::prelude::*;

use crate::app::App;
use crate::epubview::{EpubAppearance, EpubView};
use crate::pdfview::PdfView;
use crate::slideview::SlideView;

// Split submodules (pure code-motion 2026-10-31):
// the open/PDF/EPUB/slides/toc/speak/chrome/search/import machinery lives in
// `reader/`, glob re-exported so every existing `crate::reader::*` path and
// `reader::prewarm` resolve unchanged.
mod chrome;
mod engine;
mod epub;
mod import;
mod open;
mod pdf;
mod search;
mod slides;
mod speak;
mod toc;

// The crate-public surface (each was `pub fn` in the single file; bin/main.rs
// calls the pending-open pair), re-exported by name so every pre-split
// `crate::reader::<item>` path — inside and outside the crate — resolves:
pub use chrome::{float_hide, float_poke, scroll_reader, set_status, show_reader_settings};
pub use epub::{appearance_from_prefs, apply_epub_appearance};
pub use import::{on_import_done, on_repair_done, pick_and_import, start_repair_sweep};
pub use open::{forget_open_book, has_pending_open, on_audiobook_opened, on_book_opened, on_ocr_done, on_ocr_progress, open_book, serve_pending_open, set_pending_chapter, set_pending_open};
pub use pdf::{on_page_text};
pub use search::{on_search_done, search, search_current_book};
pub use slides::{apply_slide_prefs, slides_open, slides_view_visible, sync_slide_present};
pub(crate) use slides::{show_pinned, sync_show_controls};
pub use speak::{in_pdf_text_mode, page_visible, pdf_text_view, pdf_view, read_along_target, set_speaking, speak_chapter, speak_open_book, speak_from_page};
pub(crate) use speak::speak_chapter_from;
pub use toc::{TocSidebar, current_book, go_to_chapter, save_position};

pub(crate) use engine::*;
pub(crate) use epub::*;
pub(crate) use open::*;
pub(crate) use pdf::*;
pub(crate) use slides::*;
pub(crate) use toc::*;

/// How long the floating transport waits after the last mouse motion before
/// it hides again.
const FLOAT_HIDE_MS: u64 = 2800;

/// The reader's widgets, built with the window.
#[derive(Clone)]
pub struct ReaderWidgets {
    /// The reader's own column: the pages stack over the audiobook display.
    /// The reader's *controls* live one level up, in the editor toolbar —
    /// a third bar between the header and the page was chrome for chrome's
    /// sake.
    pub box_: gtk4::Box,
    /// The reader's controls, packed into the window's single toolbar row by
    /// `window::build_window` and shown only while the reader page is: the
    /// note tools and these share one row, one set visible at a time.
    pub controls: gtk4::Box,
    /// The book's own surface: a web engine showing the publisher's XHTML
    /// with the publisher's stylesheet applying. This is what an EPUB
    /// opens on — the book as the book.
    pub epub: EpubView,
    /// The engine surface for the headless documents — the Mobipocket/
    /// Kindle family, FB2, RTF, HTML and plain text. foliate-js in a web
    /// engine, the file's own bytes in one end and the book as the book out
    /// the other: real chapters, real images, the decompression Foliate
    /// has shipped for years. See `bookview.rs`.
    pub book: crate::bookview::BookView,
    /// The pages/book/text switcher. A PDF shows its rendered pages or its
    /// extracted text; an EPUB shows the book or its parsed text for
    /// read-along. The audiobook's page hides this whole stack, so the two
    /// are never on screen together.
    pub stack: gtk4::Stack,
    /// The EPUB's read-along text: the parsed chapter, in a read-only text
    /// view, because the read-along highlight is a buffer tag and a label
    /// cannot carry one.
    pub epub_text: gtk4::TextView,
    /// The scroller around the EPUB's read-along text, kept so the keyboard
    /// can scroll the page the arrows land on.
    pub epub_text_scroll: gtk4::ScrolledWindow,
    /// The PDF's read-along text: the same shape, for a PDF's extracted
    /// text.
    pub pdf_text: gtk4::TextView,
    /// The scroller around the PDF's extracted text, for the same reason.
    pub pdf_text_scroll: gtk4::ScrolledWindow,
    /// The PDF's page view: the rendered pages, one continuous scroll.
    pub pdf: PdfView,
    /// The deck's own surface: a web engine showing every slide as the deck
    /// laid it out. This is what a pptx/odp opens on.
    pub slides: SlideView,
    /// The deck's read-along text: the open slide's speakable text, the same
    /// shape as the EPUB's and PDF's.
    pub slides_text: gtk4::TextView,
    /// The scroller around the deck's read-along text.
    pub slides_text_scroll: gtk4::ScrolledWindow,
    /// The PDF's own controls, hidden for every other format.
    pub pdf_tools: gtk4::Box,
    /// The PDF's page-progress bar: how far into the document the reader
    /// is. Hidden for every other format.
    pub page_bar: gtk4::ProgressBar,
    /// The EPUB's own controls, hidden for every other format.
    pub epub_tools: gtk4::Box,
    /// Pages ⇄ text (a PDF) or book ⇄ read-along (an EPUB). One button,
    /// because the reader shows one thing or the other.
    pub mode_toggle: gtk4::Button,
    ///  the presenter toggle. Deck-only, hidden otherwise:
    /// starts fill-the-window (presentation) and opens the speaker's window.
    pub present: gtk4::Button,
    /// Force OCR: recognition for every page of the open PDF, even where
    /// the text layer passes the gate, mixed in under the layer's own text.
    pub force_ocr: gtk4::ToggleButton,
    pub zoom_out: gtk4::Button,
    pub zoom_fit: gtk4::Button,
    pub zoom_in: gtk4::Button,
    pub zoom_original: gtk4::Button,
    pub title: gtk4::Label,
    pub status: gtk4::Label,
    pub prev: gtk4::Button,
    pub next: gtk4::Button,
    pub toc: gtk4::Button,
    /// The reader's speak button, whose glyph crossfades between play and
    /// pause.
    pub speak: crate::motion::Glyph,
    /// Delete the open book. Books had no delete path — the store had one
    /// and no caller — and this is the reader's half of the one the shelf's
    /// right-click shares. Visible only while the reader page is, which is
    /// only ever true with a book open.
    pub delete: gtk4::Button,
    /// The audiobook player's elapsed/length readout, sharing the reader's
    /// status slot: only one of the two is visible at a time.
    pub abook_position: gtk4::Label,
    /// The audiobook display: cover, metadata, and its own transport. A page
    /// of the reader stack, shown instead of the chapter text while a book
    /// plays.
    pub abook: AbookWidgets,
    /// The contents sidebar's panel (see `reader/toc.rs`): the revealer the
    /// Contents button slides. The rows and the book they belong to live on
    /// the App (`app.toc_sidebar`) — a `Widgets` clone deep-copies its
    /// `RefCell`s, so state stored here would be lost with the clone.
    pub toc_revealer: gtk4::Revealer,
    /// The sidebar's rows container. Emptied and refilled when a book
    /// opens — one widget, rebuilt in place.
    pub toc_list: gtk4::Box,
    /// The floating transport for fill-the-window mode: previous/next and
    /// the page readout, shown over the page on mouse motion and gone again
    /// when the mouse rests. The toolbar row it mirrors is hidden with the
    /// mode, and without this a reader in that mode has no next button.
    pub float: gtk4::Revealer,
    /// The pill's readout — a mirror of the toolbar status label, which the
    /// mode hides.
    pub float_status: gtk4::Label,
    /// The PowerPoint show (2026-11-12): the pill's end-show button.
    /// A **mirror** show has no console — the pinned pill is the speaker's
    /// only on-screen control — so the pill carries the exit the console's
    /// "End show" would otherwise own. Visible only while such a show runs
    /// (`sync_show_controls`), so speaker mode never has two "End show"s.
    pub float_end: gtk4::Button,
}

/// The audiobook display's widgets.
#[derive(Clone)]
pub struct AbookWidgets {
    /// The page itself, a child of the reader stack.
    pub page: gtk4::Box,
    pub cover: gtk4::Picture,
    /// The placeholder drawn while the cover is missing or loading.
    pub cover_placeholder: gtk4::Box,
    pub title: gtk4::Label,
    pub author: gtk4::Label,
    pub chapter: gtk4::Label,
    pub position: gtk4::Label,
    /// The chapter-scoped slider: how far into *this chapter* the listener
    /// is, and a handle to drag there. Whole-book progress lives in the
    /// transport bar at the window's foot.
    pub chapter_bar: crate::abook::ScrubSlider,
    /// The audiobook display's play/pause button, whose glyph crossfades.
    pub play: crate::motion::Glyph,
    pub prev: gtk4::Button,
    pub next: gtk4::Button,
    pub stop: gtk4::Button,
    pub speed: gtk4::SpinButton,
}

///  start the reader's two WebKit web
/// processes NOW, not on the first book or deck open. Each view owns its
/// own `WebContext`, so each pays its own web-process spawn — measured at
/// ~1.4 s of an EPUB/deck cold open on this box — and the user's usual
/// path (launch, browse, then open) pays it mid-click. One throwaway
/// `about:blank` load each, on an idle that runs at the first main-loop
/// turn (the spawn is out-of-process; the first frame is not delayed).
/// Safety: the epub's load handler reports a non-spine URI as `None`,
/// which the reader's `on_load` ignores, and the deck's `on_loaded`
/// re-applies guards that idle without a deck — a blank page is a no-op
/// in both. The `uri()` guard skips a view whose load was already started
/// (a CLI `--open-book` races the idle), so a prewarm load can never
/// replace a real one.
pub fn prewarm(w: &ReaderWidgets) {
    use webkit6::prelude::WebViewExt;
    let epub = w.epub.widget().clone();
    let slides = w.slides.widget().clone();
    glib::idle_add_local_once(move || {
        for view in [&epub, &slides] {
            if view.uri().is_none_or(|u| u.is_empty()) {
                view.load_uri("about:blank");
            }
        }
    });
}

/// Build the reader page and its controls. Hidden until a book is open.
pub fn build(app: &Rc<App>) -> ReaderWidgets {
    // The page is a row now: the contents sidebar slides in at its left
    // (see `reader/toc.rs`), the book fills the rest. The sidebar keeps its
    // own width; every page widget lives in `inner`, so the existing
    // references — the stack, the page bar, the audiobook display — hold.
    let box_ = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(0)
        .css_classes(["reader-page"])
        .build();
    let (toc_revealer, _toc_panel, toc_list) = build_shell();
    let inner = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(0)
        .hexpand(true)
        .build();
    // Fixed 2026-11-02: the page **holds the keyboard
    // focus**. GTK4 delivers a key event along the focused widget's
    // ancestor chain, and a window with *no* focused widget delivers no
    // key event at all — so neither the reader's own capture-phase
    // controller nor the window's bubble-phase one could fire until the
    // reader happened to click an on-screen button (that click is what
    // first gave the window a focus widget: the user's own report, and
    // the AT-SPI tree of the live app, which had no focused node at all).
    //
    // A `GtkBox` is not focusable by default and `grab_focus` on a
    // non-focusable widget is a no-op, so the box asks for it. The CSS
    // (`theme.css`) keeps the focus ring out of sight: the page is not a
    // control, it is the thing the keys act on.
    box_.set_focusable(true);

    // --- the reader's controls ---------------------------------------------
    // Not a bar of their own: the note toolbar and the book controls share
    // one row (the window packs this box into it), because a second strip
    // between the header and the page repeated what the first could hold.
    // Plain box, no "toolbar" class — the row it joins already carries it.
    let controls = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(4)
        .build();

    let prev = gtk4::Button::from_icon_name("go-previous-symbolic");
    prev.set_tooltip_text(Some("Previous chapter"));
    let next = gtk4::Button::from_icon_name("go-next-symbolic");
    next.set_tooltip_text(Some("Next chapter"));

    // The book's title. It is ellipsized, and it also has a *maximum* width:
    // an ellipsized label still reports the full width of its text as its
    // natural width, so a long title would make the toolbar row wider than
    // the window and the controls would be pushed out of reach. The cap
    // keeps the title a title — long enough to identify the book, short
    // enough to leave the buttons alone.
    let title = gtk4::Label::builder()
        .halign(gtk4::Align::Start)
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        .hexpand(true)
        .max_width_chars(40)
        .width_request(120)
        .build();

    let toc = gtk4::Button::builder()
        //  icon exchanged with the mode toggle's book face per the
        // user's report — the authored view-contents glyph reads as a text
        // page (read-along) and the list glyph as a chapter list, so each
        // button wears the one that matches what it opens. A plain button
        // now: the contents is a sidebar it slides, not a popover it hosts.
        .icon_name("view-list-symbolic")
        .build();
    toc.set_tooltip_text(Some("Contents"));
    // The accessible name is pinned: a button's label is its icon otherwise.
    toc.update_property(&[gtk4::accessible::Property::Label("Contents")]);
    {
        // The contents is a column of the page now (see `reader/toc.rs`),
        // not a popover: this button slides it in and out. The MenuButton
        // this replaces floated a separate X surface, beyond the a11y
        // tree's click paths.
        let app = app.clone();
        toc.connect_clicked(move |_| toc_toggle(&app));
    }

    // The status line. Same cap as the title: it is a page number or a
    // match count, and neither needs more than a few dozen characters.
    let status = gtk4::Label::builder()
        .halign(gtk4::Align::End)
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        .max_width_chars(28)
        .css_classes(["status"])
        .build();
    // The audiobook's elapsed/length readout, sharing the reader's status
    // slot: only one of the two is visible at a time.
    let abook_position = gtk4::Label::builder()
        .halign(gtk4::Align::End)
        .css_classes(["status"])
        .visible(false)
        .build();

    let speak = crate::motion::Glyph::new();
    speak.button.set_tooltip_text(Some("Speak this chapter (F5)"));

    // The book's delete. The notes have a toolbar delete; this is the
    // books' equivalent, and it hides with the reader page because it is
    // the *open* book's delete — the shelf's right-click is the one that
    // works without opening anything.
    let delete = gtk4::Button::from_icon_name("user-trash-symbolic");
    delete.set_tooltip_text(Some("Delete this book…"));
    delete.set_visible(false);

    // --- the PDF's own controls -------------------------------------------
    // Hidden for every format but a PDF: an EPUB has no pages to zoom, and a
    // button that does nothing is worse than no button.
    let pdf_tools = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(2)
        .css_classes(["pdf-tools"])
        .visible(false)
        .build();

    let zoom_out = gtk4::Button::from_icon_name("zoom-out-symbolic");
    zoom_out.set_tooltip_text(Some("Zoom out"));
    let zoom_fit = gtk4::Button::from_icon_name("zoom-fit-best-symbolic");
    zoom_fit.set_tooltip_text(Some("Fit the page to the window"));
    let zoom_in = gtk4::Button::from_icon_name("zoom-in-symbolic");
    zoom_in.set_tooltip_text(Some("Zoom in"));
    let zoom_original = gtk4::Button::from_icon_name("zoom-original-symbolic");
    zoom_original.set_tooltip_text(Some("Original size (100%)"));

    pdf_tools.append(&zoom_out);
    pdf_tools.append(&zoom_fit);
    pdf_tools.append(&zoom_in);
    pdf_tools.append(&zoom_original);

    // A labelled toggle, not an icon: the control is a mode, and the label
    // is also what the accessibility tree — and the GUI driver — reads.
    let force_ocr = gtk4::ToggleButton::with_label("Force OCR");
    force_ocr.set_tooltip_text(Some(
        "Recognition leads the next 12 pages (the text layer only fills its gaps). Click again to stop it.",
    ));
    pdf_tools.append(&force_ocr);

    // --- the mode toggle ---------------------------------------------------
    // One button for both formats, because the reader shows one thing or the
    // other: a PDF's rendered pages or its extracted text, an EPUB's book or
    // its read-along text. It lives in its own box so the format decides what
    // sits beside it — the zoom family for a PDF, the appearance button for an
    // EPUB — without the button itself ever disappearing.
    //  initial face exchanged with the TOC button's icon (user report).
    let mode_toggle = gtk4::Button::from_icon_name("view-contents-symbolic");
    mode_toggle.set_tooltip_text(Some("Read the extracted text instead of the pages"));

    //  the presenter. Fill-the-window *is* presentation, for
    // a deck — this button starts the show: on two screens it asks Mirror or
    // Speaker view (the slide fullscreen on both, or the console here and
    // the slide fullscreen on the other screen); on one, the console floats.
    // Deck-only; hidden for a PDF, an EPUB or a note.
    let present = gtk4::Button::from_icon_name("x-office-presentation-symbolic");
    present.set_tooltip_text(Some("Present the deck"));
    present.set_visible(false);

    // --- the EPUB's own controls -------------------------------------------
    // Hidden for every other format. The button beside the mode toggle is the
    // appearance one: the book's theme, font, size, spacing and margins, in
    // the settings window's Book reader page.
    let epub_tools = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(2)
        .css_classes(["pdf-tools"])
        .visible(false)
        .build();
    let appearance_button = gtk4::Button::builder()
        .icon_name("preferences-desktop-font-symbolic")
        .build();
    appearance_button.set_tooltip_text(Some("Appearance — the book's theme, font, size and margins"));
    epub_tools.append(&appearance_button);

    controls.append(&prev);
    controls.append(&next);
    controls.append(&title);
    controls.append(&toc);
    controls.append(&mode_toggle);
    controls.append(&present);
    controls.append(&pdf_tools);
    controls.append(&epub_tools);
    controls.append(&speak.button);
    controls.append(&delete);
    controls.append(&status);
    controls.append(&abook_position);

    // --- the EPUB's book and its read-along text ----------------------------
    // Two pages of one stack: **the book** for the eyes — the publisher's
    // own XHTML in a web engine, with the publisher's own stylesheet
    // applying — and the *parsed* text for the voice. The read-along
    // highlight is a tag on a text buffer, so the text side must be a text
    // view and not a column of labels.
    let epub = EpubView::new();
    // The engine for the headless documents: the same web-engine treatment
    // the EPUB gets, through foliate-js. Its callbacks wire here, once —
    // they hold the app, and the engine outlives any single book.
    let book = crate::bookview::BookView::new();
    wire_engine_callbacks(app, &book);
    let epub_text = gtk4::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .wrap_mode(gtk4::WrapMode::Word)
        .left_margin(28)
        .right_margin(28)
        .top_margin(18)
        .bottom_margin(24)
        .css_classes(["pdf-text"])
        .build();
    let epub_text_scroll = gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vexpand(true)
        .child(&epub_text)
        .build();
    // The chapter's text view speaks from wherever the listener points:
    // right-click → "Speak from here".
    crate::player::attach_speak_from_here(app, &epub_text, true);
    //  and a selection's own affordance — the pill
    // beside a finished mouse selection, plus the F5 path through the same
    // offset logic.
    crate::speaksel::attach_selection_affordance(app, &epub_text, true);

    // --- the PDF's pages and its read-along text ----------------------------
    // The same shape: the rendered pages for the eyes, the extracted text
    // for the voice.
    let pdf = PdfView::new();
    let pdf_text = gtk4::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .wrap_mode(gtk4::WrapMode::Word)
        .left_margin(28)
        .right_margin(28)
        .top_margin(18)
        .bottom_margin(24)
        .css_classes(["pdf-text"])
        .build();
    let pdf_text_scroll = gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vexpand(true)
        .child(&pdf_text)
        .build();
    crate::player::attach_speak_from_here(app, &pdf_text, true);
    //  selection pill on the PDF's text page.
    crate::speaksel::attach_selection_affordance(app, &pdf_text, true);

    // --- the deck's slides and their read-along text ------------------------
    // The same shape again: the deck as the deck — every slide laid out in a
    // web engine — for the eyes, the slide's speakable text for the voice.
    let slides = SlideView::new();
    let slides_text = gtk4::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .wrap_mode(gtk4::WrapMode::Word)
        .left_margin(28)
        .right_margin(28)
        .top_margin(18)
        .bottom_margin(24)
        .css_classes(["pdf-text"])
        .build();
    let slides_text_scroll = gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vexpand(true)
        .child(&slides_text)
        .build();
    crate::player::attach_speak_from_here(app, &slides_text, true);
    //  selection pill on the slide-text page.
    crate::speaksel::attach_selection_affordance(app, &slides_text, true);

    // Speak-from-selection: the webview surfaces
    // report their selections through their injected scripts — the book
    // page's bubble posts the selected text (located in the chapter text by
    // search), the deck's posts the exact data-s byte its DOM carries.
    {
        let app = app.clone();
        epub.on_selection(move |speak_now, text| {
            crate::speaksel::on_epub_selection(&app, speak_now, &text);
        });
    }
    {
        let app = app.clone();
        slides.on_selection(move |speak_now, slide| {
            crate::speaksel::on_deck_selection(&app, speak_now, slide);
        });
    }
    // The PDF's pages are images with no selectable text: there, the click
    // on a page *is* the selection, and speech starts from that page.
    crate::speaksel::attach_pdf_page_speak(app, &pdf.scroll, &pdf);

    // The reader's own pages crossfade too: the book, the read-along text,
    // the PDF's pages and the PDF's text. Switching between a rendered page
    // and a column of text is a change of *kind*, and a hard cut makes it
    // read as a different document rather than the same book seen another
    // way.
    let stack = gtk4::Stack::builder()
        .vexpand(true)
        .transition_type(gtk4::StackTransitionType::Crossfade)
        .transition_duration(crate::motion::FADE_MS)
        .build();
    stack.add_titled(epub.widget(), Some("book"), "Book");
    stack.add_titled(book.widget(), Some("engine"), "Book engine");
    stack.add_titled(&epub_text_scroll, Some("text"), "Read along");
    stack.add_titled(&pdf.scroll, Some("pages"), "Pages");
    stack.add_titled(&pdf_text_scroll, Some("pdf-text"), "PDF text");
    // The deck's webview goes in behind a horizontally-scrolling wrapper:
    // the engine reports the document's own width as the widget's minimum,
    // and a slide is a fixed-width block — unwrapped, the reader's pane can
    // never shrink below the slide it shows (and fill-the-window can never
    // expand it: the divider clamps at the document's width). The wrapper's
    // `Automatic` decouples the two — the pane is the pane's, and only a
    // pane narrower than the document's minimum ever sees a scrollbar. The
    // deck scrolls itself inside either way.
    let slides_scroll = gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Automatic)
        .vscrollbar_policy(gtk4::PolicyType::Never)
        .child(slides.widget())
        .build();
    stack.add_titled(&slides_scroll, Some("slides"), "Slides");
    stack.add_titled(&slides_text_scroll, Some("slides-text"), "Slide text");

    // The PDF's own page-progress bar: a slim strip under the controls,
    // showing how far into the document the reader is.
    //
    // **Why it is separate from the status label.** The status says
    // "page 741 / 1295" — a number the reader has to read and compare. The
    // bar says the same thing *positionally*, which is what a progress bar
    // is for, and it does it without taking a line of the page's height to
    // do it: 6 px, the same height as every other bar in the app.
    //
    // **Why it glows.** It carries the same `live` class as the transport
    // bars, so while the voice is speaking it pulses with the accent — the
    // reader can see the book moving without watching the page number.
    let page_bar = gtk4::ProgressBar::builder()
        .css_classes(["pdf-page-bar"])
        .build();
    inner.append(&page_bar);

    let abook = build_abook_page(app);
    box_.append(&toc_revealer);
    box_.append(&inner);
    inner.append(&stack);
    inner.append(&abook.page);
    {
        let app = app.clone();
        prev.connect_clicked(move |_| go_to_chapter(&app, -1));
    }
    {
        let app = app.clone();
        next.connect_clicked(move |_| go_to_chapter(&app, 1));
    }
    {
        let app = app.clone();
        // The same button is the audiobook's play/pause when one is open —
        // live or stopped-on-this-page, `maybe_toggle` resumes the stopped
        // one from its bookmark rather than reading the (empty) chapter text
        // aloud over the top.
        speak.button.connect_clicked(move |_| {
            if !crate::abook::maybe_toggle(&app) {
                speak_chapter(&app);
            }
        });
    }
    {
        let app = app.clone();
        mode_toggle.connect_clicked(move |_| toggle_pdf_mode(&app));
    }
    {
        let app = app.clone();
        //  the presenter toggle — fill-the-window (which is
        // presentation, for a deck) plus the speaker's window.
        present.connect_clicked(move |_| toggle_present(&app));
    }
    {
        let app = app.clone();
        // The guard against our own reflection: `open_pdf` sets the toggle
        // from the book's marker, and a toggle that fires on that would
        // rewrite the same marker and spawn a warm-up on every open.
        force_ocr.connect_toggled(move |button| {
            let on = button.is_active();
            let Ok(Some(book)) = current_book(&app) else {
                return;
            };
            if book.force_ocr() == on {
                return;
            }
            book.set_force_ocr(on);
            if on {
                warm_ocr(&app, &book.manifest.id);
                set_status(
                    &app,
                    "Force OCR on: recognition leads the next 12 pages — click again to stop.",
                );
            } else {
                // Toggling off stops the walk in flight: the flag the
                // running warm-up checks between pages is set, so a sweep
                // ends at the next page boundary instead of running to the
                // end of the book behind a switch that says it is off.
                // `take` hands back the running walk's flag and leaves a
                // fresh (unset) one for the next walk.
                let stop = app.ocr_warmup_stop.take();
                stop.store(true, std::sync::atomic::Ordering::Relaxed);
                set_status(&app, "Force OCR off: the page's text layer leads again.");
            }
        });
    }
    {
        let app = app.clone();
        // A MenuButton opens its own popover on click; with no popover set,
        // nothing happens. So the button's job is to open the settings
        // window's Book reader page, which is where the book's typography
        // lives — one place, not two that can disagree.
        appearance_button.connect_clicked(move |_| show_reader_settings(&app));
    }
    {
        let app = app.clone();
        // One zoom family for both formats that zoom: a PDF's rendered page
        // and a deck's fit. Which one the buttons drive follows the open
        // book's format, so the buttons never need rewiring.
        zoom_out.connect_clicked(move |_| zoom_step(&app, -1));
    }
    {
        let app = app.clone();
        // The open book's delete, through the one confirmation every book
        // delete takes. The book itself comes from the cached library, the
        // same shelf-side read the row builders use — no manifest re-read.
        delete.connect_clicked(move |_| {
            let Some(id) = app.reader.borrow().book_id.clone() else {
                return;
            };
            let Ok(books) =
                sn_store::books::library_cached(&app.library, &app.library_dirty)
            else {
                return;
            };
            if let Some(book) = books.into_iter().find(|b| b.manifest.id == id) {
                crate::dialogs::confirm_delete_book(&app, book);
            }
        });
    }
    {
        let app = app.clone();
        zoom_in.connect_clicked(move |_| zoom_step(&app, 1));
    }
    {
        let app = app.clone();
        zoom_fit.connect_clicked(move |_| zoom_fit_cycle(&app));
    }
    {
        let app = app.clone();
        zoom_original.connect_clicked(move |_| {
            let Some(w) = app.widgets.borrow().clone() else {
                return;
            };
            w.reader.pdf.zoom_original();
        });
    }

    // --- the floating transport ---------------------------------------------
    // Fill-the-window hides the whole toolbar row — that is its point — but
    // a reader in that mode still has to turn pages and know where the
    // scroll stands. A small pill floats over the page's foot: it appears
    // when the mouse moves, and goes away when the mouse rests, the way a
    // video player's controls do. The readout mirrors the toolbar's status
    // label (hidden with the row) through a slow poll while revealed.
    let float_prev = gtk4::Button::from_icon_name("go-previous-symbolic");
    float_prev.set_tooltip_text(Some("Previous chapter"));
    float_prev.add_css_class("flat");
    let float_status = gtk4::Label::builder().css_classes(["numeric"]).build();
    let float_next = gtk4::Button::from_icon_name("go-next-symbolic");
    float_next.set_tooltip_text(Some("Next chapter"));
    float_next.add_css_class("flat");
    // The show's exit, riding the pill: in mirror mode the pill is pinned
    // for the whole show and there is no console to end it from. The stop
    // glyph reads as "end the show" beside the stepping arrows; the tooltip
    // is the accessible name the drive presses.
    let float_end = gtk4::Button::from_icon_name("media-playback-stop-symbolic");
    float_end.set_tooltip_text(Some("End show"));
    float_end.add_css_class("flat");
    float_end.set_visible(false);
    let float_row = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(6)
        .css_classes(["floating-transport"])
        .build();
    float_row.append(&float_prev);
    float_row.append(&float_status);
    float_row.append(&float_next);
    float_row.append(&float_end);
    let float = gtk4::Revealer::builder()
        .child(&float_row)
        .transition_type(gtk4::RevealerTransitionType::Crossfade)
        .transition_duration(260)
        .reveal_child(false)
        .halign(gtk4::Align::Center)
        .valign(gtk4::Align::End)
        .margin_bottom(22)
        .build();
    {
        let app = app.clone();
        float_prev.connect_clicked(move |_| go_to_chapter(&app, -1));
    }
    {
        let app = app.clone();
        float_next.connect_clicked(move |_| go_to_chapter(&app, 1));
    }
    {
        // The pill's End show is the console button's twin: both leave
        // fill-the-window, and `sync_slide_present` ends the show with it.
        let app = app.clone();
        float_end.connect_clicked(move |_| crate::window::apply_focus_mode(&app, false));
    }
    {
        // Motion over the pill itself counts as reaching for it: without
        // this the hide timer can fire between two clicks on it.
        let app = app.clone();
        let motion = gtk4::EventControllerMotion::new();
        motion.connect_motion(move |_, _, _| float_poke(&app));
        float_row.add_controller(motion);
    }
    {
        // The mirror. A 400 ms poll over two labels costs nothing next to
        // the render pass that produced the text, and it catches every
        // writer of the status label — the page-turn handlers, the search,
        // the zoom — without each of them having to know about the pill.
        let app = app.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(400), move || {
            if app.focus_mode.get() {
                if let Some(w) = app.widgets.borrow().clone() {
                    if w.reader.float.reveals_child() {
                        w.reader.float_status.set_text(&w.reader.status.text());
                    }
                }
            }
            glib::ControlFlow::Continue
        });
    }

    // Fixed 2026-11-01: the page's capture-phase key
    // controller — the arrows and the transport keys work whatever widget
    // holds the focus, the book's webview included. See `install_page_keys`.
    chrome::install_page_keys(app, &box_);

    ReaderWidgets {
        box_,
        controls,
        stack,
        epub,
        book,
        epub_text,
        epub_text_scroll,
        epub_tools,
        pdf_text,
        pdf_text_scroll,
        pdf,
        slides,
        slides_text,
        slides_text_scroll,
        pdf_tools,
        page_bar,
        mode_toggle,
        present,
        force_ocr,
        zoom_out,
        zoom_fit,
        zoom_in,
        zoom_original,
        title,
        status,
        prev,
        next,
        toc,
        speak,
        delete,
        abook_position,
        abook,
        toc_revealer,
        toc_list,
        float,
        float_status,
        float_end,
    }
}

/// The audiobook display: the cover, what is playing, and its transport.
///
/// The iOS app shows a book this way — a big cover, the title and author, and
/// the controls beneath it — and a transport bar at the top of the window is
/// not a substitute for it. This is the same idea in GTK: one page of the
/// reader stack, shown while an audiobook plays and hidden for an EPUB.
fn build_abook_page(app: &Rc<App>) -> AbookWidgets {
    let page = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(6)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(28)
        .margin_end(28)
        .build();
    page.set_vexpand(true);

    // The cover, centred, with a placeholder behind it for a book whose art
    // never made it into the library.
    let cover_placeholder = gtk4::Box::builder()
        .width_request(220)
        .height_request(220)
        .halign(gtk4::Align::Center)
        .valign(gtk4::Align::Center)
        .css_classes(["book-cover-placeholder", "abook-cover"])
        .build();
    cover_placeholder.append(&gtk4::Image::from_icon_name("audiobook-symbolic"));
    let cover = gtk4::Picture::builder()
        .can_shrink(true)
        .halign(gtk4::Align::Center)
        .content_fit(gtk4::ContentFit::ScaleDown)
        .css_classes(["abook-cover"])
        .build();
    cover.set_visible(false);
    page.append(&cover_placeholder);
    page.append(&cover);

    let title = gtk4::Label::builder()
        .halign(gtk4::Align::Center)
        .wrap(true)
        .justify(gtk4::Justification::Center)
        .max_width_chars(48)
        .css_classes(["abook-title"])
        .build();
    let author = gtk4::Label::builder()
        .halign(gtk4::Align::Center)
        .wrap(true)
        .css_classes(["abook-author"])
        .build();
    let chapter = gtk4::Label::builder()
        .halign(gtk4::Align::Center)
        .wrap(true)
        .justify(gtk4::Justification::Center)
        .max_width_chars(48)
        .css_classes(["abook-chapter"])
        .build();
    let position = gtk4::Label::builder()
        .halign(gtk4::Align::Center)
        .css_classes(["abook-position", "numeric"])
        .build();
    page.append(&title);
    page.append(&author);
    page.append(&chapter);
    page.append(&position);

    // The chapter-scoped slider, between the readout and the transport: how
    // far into this chapter the listener is, and a handle to drag there.
    // The whole-book fill is the transport bar's slider at the window's
    // foot — one slider per scope, never two numbers on one.
    let chapter_bar = crate::abook::ScrubSlider::new(app, false);
    chapter_bar.add_css_class("abook-chapter-bar");
    page.append(chapter_bar.widget());

    // The transport, centred under the readout. The ±15s skips sit between
    // the chapter steppers and play, the pair a podcast app puts there: a
    // seek is a respawn, so they go through `abook::skip` like any other
    // re-open.
    let controls = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(12)
        .halign(gtk4::Align::Center)
        .margin_top(12)
        .css_classes(["abook-controls"])
        .build();
    let prev = gtk4::Button::from_icon_name("media-skip-backward-symbolic");
    prev.set_tooltip_text(Some("Previous chapter"));
    prev.add_css_class("abook-button");
    let back15 = gtk4::Button::with_label("-15");
    back15.set_tooltip_text(Some("Skip back 15 seconds"));
    back15.add_css_class("abook-button");
    let play = crate::motion::Glyph::new();
    play.button.set_tooltip_text(Some("Play or pause (F5)"));
    play.button.add_css_class("abook-button");
    play.button.add_css_class("abook-play");
    // The audiobook's display page only exists while a book is open, so the
    // button starts on the pause glyph — it is what it will be asked to do
    // first.
    play.set_pause();
    let fwd15 = gtk4::Button::with_label("+15");
    fwd15.set_tooltip_text(Some("Skip forward 15 seconds"));
    fwd15.add_css_class("abook-button");
    let next = gtk4::Button::from_icon_name("media-seek-forward-symbolic");
    next.set_tooltip_text(Some("Next chapter"));
    next.add_css_class("abook-button");
    let stop = gtk4::Button::from_icon_name("media-playback-stop-symbolic");
    stop.set_tooltip_text(Some("Stop (F6)"));
    stop.add_css_class("abook-button");
    controls.append(&prev);
    controls.append(&back15);
    controls.append(&play.button);
    controls.append(&fwd15);
    controls.append(&next);
    controls.append(&stop);
    page.append(&controls);

    let speed = gtk4::SpinButton::with_range(0.5, 2.0, 0.05);
    speed.set_digits(2);
    // Seeded from the audiobook's own pref, and recorded in `shown_speed` as
    // what the display shows — the same memory the transport bar's spin uses,
    // so a programmatic set in either is not mistaken for the dial turning.
    speed.set_value(app.prefs.get().audio_rate);
    app.shown_speed.set(app.prefs.get().audio_rate);
    speed.set_tooltip_text(Some("Playback speed"));
    speed.set_width_request(110);
    let speed_row = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk4::Align::Center)
        .margin_top(8)
        .build();
    speed_row.append(&gtk4::Label::new(Some("Speed")));
    speed_row.append(&speed);
    page.append(&speed_row);

    {
        let app = app.clone();
        // Play after a stop re-opens the book from its bookmark — the display
        // outlives its session, so the button must work without one.
        play.button.connect_clicked(move |_| {
            crate::abook::maybe_toggle(&app);
        });
    }
    {
        let app = app.clone();
        stop.connect_clicked(move |_| crate::abook::close(&app));
    }
    {
        let app = app.clone();
        prev.connect_clicked(move |_| {
            if let Some(session) = app.abook.borrow().clone() {
                if session.chapter_index > 0 {
                    crate::abook::open(
                        &app,
                        &session.book_id,
                        session.chapter_index - 1,
                        0,
                        app.prefs.get().audio_rate,
                    );
                }
            }
        });
    }
    {
        let app = app.clone();
        next.connect_clicked(move |_| {
            if let Some(session) = app.abook.borrow().clone() {
                crate::abook::open(
                    &app,
                    &session.book_id,
                    session.chapter_index + 1,
                    0,
                    app.prefs.get().audio_rate,
                );
            }
        });
    }
    {
        let app = app.clone();
        back15.connect_clicked(move |_| crate::abook::skip(&app, -15.0));
    }
    {
        let app = app.clone();
        fwd15.connect_clicked(move |_| crate::abook::skip(&app, 15.0));
    }
    {
        let app = app.clone();
        speed.connect_value_changed(move |spin| {
            let value = spin.value();
            // A programmatic set — showing the live rate when a book opens —
            // must not read as the listener turning the dial.
            if (value - app.shown_speed.get()).abs() < 0.001 {
                return;
            }
            app.prefs.update(|p| p.audio_rate = value);
            app.shown_speed.set(value);
            if crate::abook::is_live(&app) {
                crate::abook::restart(&app);
            }
        });
    }

    page.set_visible(false);
    AbookWidgets {
        page,
        cover,
        cover_placeholder,
        title,
        author,
        chapter,
        position,
        chapter_bar,
        play,
        prev,
        next,
        stop,
        speed,
    }
}

/// The open book, and where in it the reader stands. UI-thread state, like
/// every other pane's.
pub struct ReaderState {
    pub book_id: Option<String>,
    pub chapter: usize,
    /// The chapter the current search's first hit is in, when the user asks
    /// to jump to it.
    pub search_first_hit: Option<usize>,
    /// The text of the open chapter, kept so the reader can search it and
    /// the speech path can resume inside it.
    pub text: String,
    /// The book's format, so the pane knows what it is showing: a PDF's
    /// pages, an EPUB's text, an audiobook's display.
    pub format: String,
    /// True when the reader is looking at the PDF's extracted text rather
    /// than its rendered pages.
    pub pdf_text_mode: bool,
    /// True while the voice is driving the view, so the page the reader
    /// scrolled to is not mistaken for the page the voice is on.
    pub speaking: bool,
    /// The voice asked for this open (an auto-advance, or play on a page
    /// with no text): when *that* open lands, the chapter starts speaking.
    /// Stamped with the open's generation, so a user's own open in between
    /// — a different generation — never inherits the voice's flag. The
    /// session must start on the opened chapter's text, which does not
    /// exist until the worker lands, never on the one that just finished.
    pub speak_when_opened: Option<u64>,
    /// The counter that tags every background request. A result whose
    /// generation is not the current one is stale — another book was asked
    /// for, another query typed — and is dropped instead of landing on top
    /// of newer state.
    pub generation: u64,
    /// The counter that tags in-book searches. It is separate from the
    /// book's generation because a search is a question about the *open*
    /// book: opening another book invalidates both, but a new query
    /// invalidates only the search.
    pub search_generation: u64,
    /// The open book renders through the engine (the book view): the
    /// relocations, the book report and the section text reports belong to
    /// this flag — a PDF or an EPUB on the per-spine view ignores them.
    pub engine_rendered: bool,
    /// The engine has finished opening the book (the shell's `ready`): a
    /// re-open of the same book navigates instead of reloading.
    pub engine_ready: bool,
    /// The reader turned the page (or the section) while the voice was
    /// reading along: the next relocation — the page the move landed on —
    /// hands the voice its new starting point. Set by the steppers and the
    /// arrow keys, consumed once, by the relocation handler.
    pub voice_follows_page: bool,
    /// How many sections the voice's own walk has passed without finding
    /// one the engine has text for. The engine's text decides — not the old
    /// parse's — so this count is what stops the walk after twenty silent
    /// sections instead of trying to prove the whole book silent.
    pub voice_walk_skips: u32,
    /// The section the voice's walk is reading or steering into, while a
    /// walk is under way. Its own authority, separate from `chapter`: the
    /// relocation reports the section the visible *page* starts in, and on a
    /// continuously-paginated book that is not the section the voice is on —
    /// the mobi's front matter is five one-line sections sharing one page.
    /// A walk that tracked `chapter` would be dragged back to the page start
    /// by every relocation and re-read the same section forever (measured
    /// 2026-11-17). `None` when no walk is under way — a user-started voice
    /// (F5, the transport, a stepper restart) clears it, and the walk
    /// resumes from `chapter`.
    pub walk_section: Option<usize>,
}

impl Default for ReaderState {
    fn default() -> Self {
        Self {
            book_id: None,
            chapter: 0,
            text: String::new(),
            search_first_hit: None,
            format: String::new(),
            pdf_text_mode: false,
            speaking: false,
            speak_when_opened: None,
            generation: 0,
            search_generation: 0,
            engine_rendered: false,
            engine_ready: false,
            voice_follows_page: false,
            voice_walk_skips: 0,
            walk_section: None,
        }
    }
}

/// Open the book at its saved position: the bookmark's chapter and — for an
/// audiobook — its millisecond offset. Every "just open it" path passes this
/// instead of a computed chapter, so the resume logic lives in `open_book`
/// alone; an explicit number always means "that chapter, from its top".
pub const RESUME: usize = usize::MAX;

/// Whether the open book renders through the engine (the book view). The
/// voice's auto-advance reads it to route its walk: an engine book's
/// sections are the engine's own, and the old parse's chapter list cannot
/// steer it.
pub fn engine_rendered(app: &Rc<App>) -> bool {
    app.reader.borrow().engine_rendered
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::search::original_offset_of;

    /// The offset of a character in the *original* text, given the offset of
    /// its folded form.
    ///
    /// Folding is NFKD + casefold, which changes byte lengths: "é" is two
    /// bytes but folds to one, and "İ" folds to two characters. The map
    /// therefore has to walk both strings in lockstep, and the whole point
    /// of the rewrite was that it does so without allocating a `String` per
    /// character.
    #[test]
    fn original_offset_is_exact_for_ascii() {
        let original = "Hello, world";
        for offset in 0..=original.len() {
            assert_eq!(original_offset_of(original, offset), offset);
        }
    }

    #[test]
    fn original_offset_survives_a_length_change() {
        // "é" is two bytes in UTF-8 and folds to one byte ("e" plus the
        // combining accent, which the fold strips). The folded offset of
        // the *next* character is therefore one less than its original
        // offset — and the map has to account for that.
        let original = "aéb";
        let folded = sn_core::util::fold(original);
        assert_ne!(folded.len(), original.len());
        // Every byte offset in the original maps to the byte offset of the
        // character that starts it.
        assert_eq!(original_offset_of(original, 0), 0);
        assert_eq!(original_offset_of(original, 1), 1);
        // The folded offset of 'b' is 2 (a, e); in the original 'b' is at
        // byte 3 (a, é-as-two-bytes).
        assert_eq!(original_offset_of(original, 2), 3);
    }

    #[test]
    fn original_offset_past_the_end_clamps() {
        let original = "short";
        assert_eq!(original_offset_of(original, 999), original.len());
    }

    #[test]
    fn original_offset_of_an_empty_string_is_zero() {
        assert_eq!(original_offset_of("", 0), 0);
        assert_eq!(original_offset_of("", 5), 0);
    }

    /// The generation counter is what makes a stale answer droppable. Two
    /// books opened in a row must not share a generation, and a search must
    /// not be invalidated by the book's own generation moving (they are
    /// separate counters for separate questions).
    #[test]
    fn generations_move_independently() {
        let mut state = ReaderState::default();
        assert_eq!(state.generation, 0);
        assert_eq!(state.search_generation, 0);
        state.generation += 1;
        assert_eq!(state.generation, 1);
        // The book's generation moved; the search's did not.
        assert_eq!(state.search_generation, 0);
        state.search_generation += 1;
        assert_eq!(state.search_generation, 1);
        assert_eq!(state.generation, 1);
    }

    /// The page-progress bar's fraction.
    ///
    /// The first page of a hundred is the *start* of the document, so the
    /// bar reads as a sliver of the whole rather than as empty — and the
    /// last page is full. A zero-page book has no fraction to show.
    #[test]
    fn page_bar_fraction_spans_the_document() {
        assert_eq!(page_bar_fraction(0, 100), 0.01);
        assert_eq!(page_bar_fraction(49, 100), 0.50);
        assert_eq!(page_bar_fraction(99, 100), 1.0);
    }

    #[test]
    fn page_bar_fraction_is_safe_at_the_edges() {
        // No pages: no division by zero.
        assert_eq!(page_bar_fraction(0, 0), 0.0);
        // A page past the end (a stale page number) clamps rather than
        // drawing a bar wider than the track.
        assert_eq!(page_bar_fraction(200, 100), 1.0);
    }
}