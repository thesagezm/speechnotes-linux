//! The PDF page viewer: real rendered pages, one continuous scroll.
//!
//! A PDF is a page description, not a chapter of prose. The iOS app drew its
//! pages; the Linux port had only ever extracted the *text* out of them and
//! shown that as labels, which is the wrong shape for a reader — a page is
//! looked at, not read line by line. This module is the page side of the
//! split the iOS app had: **images for the eyes, text for the voice.** The
//! extracted text stays where it belongs — the speech path, the search, the
//! read-along highlight.
//!
//! How a 1295-page book stays usable:
//!
//! * **Layout comes from the page boxes, not from the images.** Every page's
//!   `/MediaBox` is known before anything is drawn (the parse hands over the
//!   list), so the scroll's geometry is a prefix sum over the page heights.
//!   No page is measured after it is rendered.
//! * **Rows are materialised lazily.** A `GtkFixed` holds one row per page
//!   *in the window*, plus a spacer the height of the whole document, so the
//!   scrollbar knows the book's length without a widget per page. A row that
//!   scrolls out of the window is dropped, texture and all.
//! * **Rendering happens off the UI thread.** `pdftoppm` (poppler) draws one
//!   page into a PNG; the worker sends the file back and the UI loads it as a
//!   texture. The page cache is on disk and keyed by the book's id and the
//!   DPI, so a second visit to a page costs nothing.
//!
//! The renderer is `pdftoppm` because it is on every box that has a PDF
//! reader, and because poppler's own C library has no development headers
//! here — bindings to it cannot be built, and a shell-out to the same code
//! can.
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver, Sender};

use gtk4::glib;
use gtk4::prelude::*;

mod jump;
mod layout;
mod pages;
mod worker;

// Re-exported wholesale: the module's public surface (`PdfView`,
// `sweep_stale_page_cache`, `FitMode`) keeps resolving; the internal
// machinery is crate-visible across the submodules.
use std::time::Duration;

use jump::{answer_pending_jump, arm_settle, page_at, perform_jump};
use layout::{relayout, sync_viewport};
use pages::{collect_done, materialise};
use sn_doc::pdf::{PageLink, PageLinkTarget};
use worker::{spawn_worker, Done, Job};

pub use worker::sweep_stale_page_cache;

const PAGE_GAP: f64 = 12.0;

/// The margin either side of a page in fit-width mode.
const PAGE_MARGIN: f64 = 16.0;

/// The room left for the vertical scrollbar when a page is fitted to the
/// pane.
const SCROLLBAR_ALLOWANCE: f64 = 18.0;

/// The render DPI's floor and ceiling. The floor is low because fit-width
/// on a narrow pane asks for less than 72: a letter page in a 450-pixel
/// pane is 55 dpi, and a page drawn at 72 would be wider than the pane.
const MIN_DPI: u32 = 36;
const MAX_DPI: u32 = 300;

/// How long the scroll must be still before the reader's page changes.
const SETTLE_MS: u64 = 400;

/// How often the viewer's own state is looked at: the worker's answers are
/// collected, and the pane's size is checked.
const POLL_MS: u64 = 60;

/// How long a scroll waits before the pages around it are wanted.
const MATERIALISE_MS: u64 = 120;

/// The zoom steps. A quarter at a time is what every PDF viewer does.
const ZOOM_STEP: f64 = 1.25;

/// How the pages are scaled to the pane.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FitMode {
    /// The widest page fills the pane's width. The default.
    Width,
    /// The tallest page fits the pane's height.
    Page,
    /// Whatever the user zoomed to.
    Free,
}

/// One rasterisation job: a page to draw, and where its PNG belongs.
struct PageRow {
    frame: gtk4::Box,
    picture: gtk4::Picture,
}

/// A jump the reader asked for that has not landed yet.
///
/// The pane is measured after the reader asks to go somewhere: the layout
/// that says where page 741 is depends on how wide the pane is, and the
/// pane's width is not known until the window has been realised. A jump
/// answered against the layout that exists at that moment lands on the
/// wrong page — on this box, page 35 landed on page 49 — because the pages
/// were taller in the layout the jump was computed against.
///
/// The jump is therefore *owed* until it lands: the next layout answers it
/// again, in the layout that is real by then.
#[derive(Clone, Copy, Debug)]
struct PendingJump {
    /// The page the reader asked for.
    page: usize,
    /// The y the jump set the scroll to, in the layout it was computed in.
    ///
    /// `None` while the jump has never been performed — there was no
    /// layout to perform it against. The y is what tells a landing on the
    /// wrong page (the layout moved under the jump) apart from the reader
    /// scrolling away (the jump is abandoned).
    y: Option<f64>,
}

/// Everything the viewer knows about the book it is showing.
struct State {
    /// The book's id — the cache is keyed by it, so two books never collide.
    book_id: String,
    /// The PDF file to rasterise.
    pdf: PathBuf,
    /// Each page's box in points, in reading order.
    sizes: Vec<(f64, f64)>,
    /// Each page's slot in logical pixels, after scaling.
    slots: Vec<(f64, f64)>,
    /// The y of each page's top, as a prefix sum over the slots.
    prefix: Vec<f64>,
    /// The whole document's height.
    total: f64,
    /// Logical pixels per point. 96/72 is "100%", the size a PDF viewer on
    /// every desktop shows a page at.
    scale: f64,
    /// How the pages are scaled.
    fit: FitMode,
    /// The display's scale factor: a HiDPI screen draws the same page twice
    /// as large, so it is rendered twice as large too.
    dsf: f64,
    /// The DPI the pages are rasterised at.
    dpi: u32,
    /// The pane's size in logical pixels.
    viewport: (f64, f64),
    /// The rows that exist right now.
    rows: HashMap<usize, PageRow>,
    /// The pages whose rendering has been asked for, at which DPI.
    requested: HashSet<(usize, u32)>,
    /// Bumped whenever the book or the zoom changes: answers from an older
    /// epoch are for a page nobody is looking at any more.
    epoch: u64,
    /// The page the reader stands on.
    current: usize,
    /// The jump the reader asked for that has not landed yet.
    pending: Option<PendingJump>,
    /// The book's own link annotations: the contents page's clickable
    /// rows. Empty until the parse's table arrives; the view renders the
    /// same either way.
    links: Vec<PageLink>,
}

/// The viewer's parts, behind one `Rc` so the scroll's own handlers can
/// reach them.
struct Inner {
    /// The scroller the pages live in.
    scroll: gtk4::ScrolledWindow,
    /// The page canvas. Children are positioned by absolute `y`.
    fixed: gtk4::Fixed,
    /// A box the height of the whole document: the scroll extent comes from
    /// it, so the scrollbar knows the book's length without a widget per
    /// page.
    spacer: gtk4::Box,
    state: RefCell<State>,
    /// The worker's queue.
    jobs: Sender<Job>,
    /// The worker's answers.
    done: RefCell<Receiver<Done>>,
    /// Called when the scroll settles on a new page.
    on_page: RefCell<Option<Rc<dyn Fn(usize)>>>,
    /// Called when the reader clicks a link that leaves the document —
    /// the desktop's safe-link rule applies outside this view.
    on_external_link: RefCell<Option<Rc<dyn Fn(&str)>>>,
    /// The pending "the scroll has settled" timer.
    settle: RefCell<Option<glib::SourceId>>,
}

/// The PDF page viewer.
#[derive(Clone)]
pub struct PdfView {
    /// The scroll window the pages live in. The reader puts it on its stack.
    pub scroll: gtk4::ScrolledWindow,
    inner: Rc<Inner>,
}

/// The link under a click at the *canvas* point `(cx, cy)`, if one is
/// there. Pure geometry, so a test can pin it: the click's page comes from
/// the prefix sums (`page_at`), the page sits centred in the canvas — the
/// same rule `place_rows` lays rows out by — and a link's rectangle is
/// page-local points, which the layout's scale turns into the same pixels
/// the page image is drawn at.
fn link_target_at(state: &State, cx: f64, cy: f64) -> Option<PageLinkTarget> {
    let page = page_at(state, cy);
    let &(slot_w, slot_h) = state.slots.get(page)?;
    let &top = state.prefix.get(page)?;
    let canvas_w = state
        .slots
        .iter()
        .map(|(w, _)| *w)
        .fold(0.0_f64, f64::max)
        .max(state.viewport.0);
    let page_x = ((canvas_w - slot_w) / 2.0).max(0.0);
    let (px, py) = (cx - page_x, cy - top);
    if !(0.0..=slot_w).contains(&px) || !(0.0..=slot_h).contains(&py) {
        return None;
    }
    state.links.iter().find_map(|link| {
        if link.page != page + 1 {
            return None;
        }
        let (rx0, ry0, rx1, ry1) = link.rect;
        let (rx0, ry0, rx1, ry1) = (
            rx0 * state.scale,
            ry0 * state.scale,
            rx1 * state.scale,
            ry1 * state.scale,
        );
        (px >= rx0 && px <= rx1 && py >= ry0 && py <= ry1).then(|| link.target.clone())
    })
}

/// The link under a primary click, if one is there. The click arrives in
/// the scroll viewport's coordinates; the document's are the scroll's own
/// position added to it.
fn link_at_click(inner: &Rc<Inner>, x: f64, y: f64) {
    let (cx, cy) = {
        let h = inner.scroll.hadjustment();
        let v = inner.scroll.vadjustment();
        (h.value() + x, v.value() + y)
    };
    match link_target_at(&inner.state.borrow(), cx, cy) {
        Some(PageLinkTarget::Page(page)) => perform_jump(inner, page.saturating_sub(1)),
        Some(PageLinkTarget::Uri(uri)) => {
            if let Some(f) = inner.on_external_link.borrow().as_ref() {
                f(&uri);
            }
        }
        None => {}
    }
}

impl PdfView {
    /// Build the viewer: a scroller over a fixed canvas, a worker thread, and
    /// the poll that collects the worker's answers.
    pub fn new() -> Self {
        let fixed = gtk4::Fixed::new();
        let spacer = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        fixed.put(&spacer, 0.0, 0.0);

        let scroll = gtk4::ScrolledWindow::builder()
            .hscrollbar_policy(gtk4::PolicyType::Automatic)
            .vscrollbar_policy(gtk4::PolicyType::Automatic)
            .vexpand(true)
            // The canvas's natural width is the widest page, and the
            // scroller would otherwise grow to it: the pane would be as
            // wide as the page, and the page would look larger than the
            // window. The scroller keeps the pane's width and scrolls the
            // canvas sideways inside it.
            .propagate_natural_width(false)
            .child(&fixed)
            .build();

        let (job_tx, job_rx) = channel::<Job>();
        let (done_tx, done_rx) = channel::<Done>();
        spawn_worker(job_rx, done_tx);

        let inner = Rc::new(Inner {
            scroll: scroll.clone(),
            fixed,
            spacer,
            state: RefCell::new(State {
                book_id: String::new(),
                pdf: PathBuf::new(),
                sizes: Vec::new(),
                slots: Vec::new(),
                prefix: Vec::new(),
                total: 0.0,
                scale: 96.0 / 72.0,
                fit: FitMode::Width,
                dsf: 1.0,
                dpi: 96,
                viewport: (0.0, 0.0),
                rows: HashMap::new(),
                requested: HashSet::new(),
                epoch: 0,
                current: 0,
                pending: None,
                links: Vec::new(),
            }),
            jobs: job_tx,
            done: RefCell::new(done_rx),
            on_page: RefCell::new(None),
            on_external_link: RefCell::new(None),
            settle: RefCell::new(None),
        });

        // The scroll's own handlers. GTK4 has no "the size changed" signal
        // for a widget's allocation, so the pane's size is read on every poll
        // instead — a comparison against the last size, and a re-layout only
        // when it really changed.
        {
            let inner = inner.clone();
            scroll.vadjustment().connect_value_changed(move |_| {
                arm_settle(&inner);
            });
        }
        // The adjustment's range catching up is the moment a clamped jump
        // becomes completable: the upper lags the layout one frame behind
        // (allocation), so a keep-page scroll set during a pane growth
        // (fullscreen, sidebar collapse) landed at the OLD upper and only
        // this signal knows the rest of the book now exists.
        {
            let inner = inner.clone();
            scroll.vadjustment().connect_changed(move |_| {
                answer_pending_jump(&inner);
            });
        }

        // The worker's answers, collected on the UI thread. A background
        // thread may not touch a widget, so the poll is the only way in.
        {
            let inner = inner.clone();
            glib::timeout_add_local(Duration::from_millis(POLL_MS), move || {
                collect_done(&inner);
                sync_viewport(&inner);
                glib::ControlFlow::Continue
            });
        }

        // The book's own links: a primary click on a page is looked up in
        // the parse's annotation table before anything else — the contents
        // page's rows jump to their pages, a web address goes to the
        // reader (the safe-link rule lives there). The right-click speak
        // gesture is a different controller on a different button, so the
        // two never meet.
        {
            let inner = inner.clone();
            let click = gtk4::GestureClick::new();
            click.set_button(1);
            click.connect_pressed(move |_, _, x, y| {
                link_at_click(&inner, x as f64, y as f64);
            });
            scroll.add_controller(click);
        }

        PdfView { scroll, inner }
    }

    /// Show a book: its id, its file, and every page's box in points.
    ///
    /// The pages are laid out immediately — the geometry is known from the
    /// boxes alone — and the window's pages are rendered as they come into
    /// view.
    pub fn set_book(&self, book_id: &str, pdf: PathBuf, sizes: Vec<(f64, f64)>) {
        {
            let mut state = self.inner.state.borrow_mut();
            state.book_id = book_id.to_string();
            state.pdf = pdf;
            state.sizes = sizes;
            state.slots.clear();
            state.prefix.clear();
            state.total = 0.0;
            state.requested.clear();
            state.epoch += 1;
            state.current = 0;
            state.pending = None;
            state.links = Vec::new();
            // A new book starts at fit-width: the reader asked for the page
            // to fill the pane, not for a zoom level from another book.
            state.fit = FitMode::Width;
        }
        self.clear_rows();
        sync_viewport(&self.inner);
        relayout(&self.inner, false);
        materialise(&self.inner);
    }

    /// Hand the viewer the book's link annotations — the parse's table, in
    /// reading order. The geometry needs nothing else: each rectangle is
    /// page-local points with the page's top-left as its origin, the same
    /// convention the rendered image has.
    pub fn set_links(&self, links: Vec<PageLink>) {
        self.inner.state.borrow_mut().links = links;
    }

    /// Call `f` when the reader clicks a link that leaves the document.
    /// The safe-link rule (which schemes may open) lives with the reader.
    pub fn on_external_link<F: Fn(&str) + 'static>(&self, f: F) {
        *self.inner.on_external_link.borrow_mut() = Some(Rc::new(f));
    }

    /// Forget the book. The rows go, the cache stays (it is keyed by the
    /// book's id, so the next visit is free).
    pub fn clear(&self) {
        {
            let mut state = self.inner.state.borrow_mut();
            state.sizes.clear();
            state.slots.clear();
            state.prefix.clear();
            state.total = 0.0;
            state.requested.clear();
            state.epoch += 1;
            state.current = 0;
            state.pending = None;
            state.links = Vec::new();
        }
        self.clear_rows();
        self.inner.spacer.set_size_request(0, 0);
    }

    /// The page the reader stands on.
    pub fn current_page(&self) -> usize {
        self.inner.state.borrow().current
    }

    /// The number of pages the book has.
    pub fn page_count(&self) -> usize {
        self.inner.state.borrow().sizes.len()
    }

    /// Call `f` when the scroll settles on a new page.
    pub fn on_page_changed<F: Fn(usize) + 'static>(&self, f: F) {
        *self.inner.on_page.borrow_mut() = Some(Rc::new(f));
    }

    /// Show a page: the scroll goes to its top.
    ///
    /// The jump is *remembered* as well as performed. A jump asked for
    /// before the pane has been measured is answered against a layout that
    /// is about to change under it: the y belongs to the old scale, and
    /// the page the reader lands on is a different one. Keeping the
    /// target lets the next relayout answer the jump again, in the layout
    /// that is now the real one.
    pub fn scroll_to_page(&self, page: usize) {
        perform_jump(&self.inner, page);
    }

    /// The page at `y` in the document's own coordinates (the scroll's
    /// value plus the pointer's viewport offset).
    ///
    /// Speak-from-selection: a rendered page has no
    /// selectable text, so the pages view's "Speak from here" is a click on
    /// the page itself — this locates the page the click landed on. Authored
    /// from the private `page_at` helper.
    pub fn page_at_doc_y(&self, y: f64) -> usize {
        let state = self.inner.state.borrow();
        page_at(&state, y)
    }

    /// Zoom in a quarter.
    pub fn zoom_in(&self) {
        let scale = self.inner.state.borrow().scale;
        self.set_zoom(scale * ZOOM_STEP);
    }

    /// Zoom out a quarter.
    pub fn zoom_out(&self) {
        let scale = self.inner.state.borrow().scale;
        self.set_zoom(scale / ZOOM_STEP);
    }

    /// Show the page at its natural size: one point to one and a third
    /// pixels, which is what every desktop shows a PDF at "100%".
    pub fn zoom_original(&self) {
        self.set_zoom(96.0 / 72.0);
    }

    /// Fit the pages to the pane: the width first, the page on a second
    /// press.
    pub fn zoom_fit(&self) {
        let mode = match self.inner.state.borrow().fit {
            FitMode::Width => FitMode::Page,
            _ => FitMode::Width,
        };
        {
            let mut state = self.inner.state.borrow_mut();
            state.fit = mode;
        }
        // The zoom re-lays every page out, so the scroll's old y lands on a
        // different page — the reader keeps the page they were on.
        relayout(&self.inner, true);
        materialise(&self.inner);
    }

    /// The mode the pages are scaled by.
    pub fn fit_mode(&self) -> FitMode {
        self.inner.state.borrow().fit
    }

    /// Set the scale directly and let the pages follow.
    fn set_zoom(&self, scale: f64) {
        {
            let mut state = self.inner.state.borrow_mut();
            state.scale = scale.clamp(0.1, 8.0);
            state.fit = FitMode::Free;
        }
        // Same keep-the-page rule as the fit cycle.
        relayout(&self.inner, true);
        materialise(&self.inner);
    }

    /// Drop every row: their textures go with them.
    fn clear_rows(&self) {
        let mut state = self.inner.state.borrow_mut();
        let pages: Vec<usize> = state.rows.keys().copied().collect();
        for page in pages {
            if let Some(row) = state.rows.remove(&page) {
                self.inner.fixed.remove(&row.frame);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A two-page layout the hit-test can be pinned against: every page
    /// 100×200 points laid out at scale 0.5, so each slot is 50×100
    /// pixels, and the second page's top sits one gap below the first's
    /// height. Two links on page 1 — one to a page, one to the web.
    fn state() -> State {
        State {
            book_id: "t".into(),
            pdf: PathBuf::new(),
            sizes: vec![(100.0, 200.0); 2],
            slots: vec![(50.0, 100.0); 2],
            prefix: vec![0.0, 112.0],
            total: 212.0,
            scale: 0.5,
            fit: FitMode::Width,
            dsf: 1.0,
            dpi: 96,
            viewport: (500.0, 400.0),
            rows: HashMap::new(),
            requested: HashSet::new(),
            epoch: 0,
            current: 0,
            pending: None,
            links: vec![
                PageLink {
                    page: 1,
                    rect: (10.0, 10.0, 90.0, 20.0),
                    target: PageLinkTarget::Page(2),
                },
                PageLink {
                    page: 1,
                    rect: (10.0, 30.0, 40.0, 40.0),
                    target: PageLinkTarget::Uri("https://example.org".into()),
                },
            ],
        }
    }

    #[test]
    fn a_click_on_a_contents_row_lands_on_its_target() {
        let state = state();
        // The canvas is the viewport (500 wide); the 50-pixel page is
        // centred at x 225. A click at the first link's centre — page
        // points (50, 15) — is canvas (250, 7.5), and it goes to page 2.
        let hit = link_target_at(&state, 250.0, 7.5).expect("the row is a link");
        assert_eq!(hit, PageLinkTarget::Page(2));
        // The web row, likewise: page points (40, 35).
        let hit = link_target_at(&state, 225.0 + 20.0, 17.5).expect("the uri row");
        assert_eq!(
            hit,
            PageLinkTarget::Uri("https://example.org".into())
        );
    }

    #[test]
    fn a_click_off_the_links_is_none() {
        let state = state();
        // The gap between the pages, the margin left of the page, and the
        // second page (which carries no links) all answer none — those
        // clicks stay free for whatever else means them.
        assert_eq!(link_target_at(&state, 250.0, 106.0), None, "the gap");
        assert_eq!(link_target_at(&state, 205.0, 7.5), None, "left of the page");
        assert_eq!(link_target_at(&state, 250.0, 112.0 + 7.5), None, "page two");
    }
}
