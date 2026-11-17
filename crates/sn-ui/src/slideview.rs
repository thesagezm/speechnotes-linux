//! The slide deck viewer: **the deck as the deck**, in a real web engine.
//!
//! The same model as the EPUB viewer (calibre's), applied to a deck:
//!
//! * Every slide is laid out as the slide — shapes absolutely positioned at
//!   their resolved points, fills, borders, runs and tables carried across
//!   — because a deck's design *is* the content. A parsed-text view of a
//!   slide is not the slide.
//! * The deck is one HTML document: every slide, so flipping through is a
//!   scroll (or, in presentation mode, a scroll-snap) rather than a page
//!   load per slide. The engine keeps one web process for the whole deck.
//! * The bytes (pictures, posters, picture fills) are served over the
//!   family's private scheme (`speechnotes://deck/...`) from the memoised
//!   parse — never from the filesystem.
//!
//! The split the reader asks for is the EPUB's: the **webview** is the deck
//! (what you look at), and the slide's speakable text feeds the voice and
//! the read-along view. The read-along highlight has two homes: the slide
//! itself (a `data-s`/`data-e` byte-range highlight in the webview, when
//! the slides are on screen) and the read-along text view (the same buffer
//! tag the other formats use, when the text side is on screen).
//!
//! The show ([`ShowKind`], in `presenter.rs`) spawns more views on the same
//! engine: the audience screen's fullscreen slide and the console's two
//! previews. One scheme handler, one deck parse, one document — every view
//! a real rendering of the slide.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use gtk4::prelude::*;
use webkit6::prelude::*;

use sn_doc::slides::SlideDeck;

// Split submodules (pure code-motion 2026-10-31): each glob re-exported at its
// own visibility so every existing `crate::slideview::*` path resolves unchanged.
mod document;
mod presenter;
mod script;
mod serve;

pub(crate) use presenter::*;
pub(crate) use serve::*;
pub(crate) use document::*;
pub(crate) use script::*;

/// The URL scheme the deck's bytes are served over. The family's one scheme;
/// the host below says which book is speaking.
pub const SCHEME: &str = "speechnotes";

/// The host under which the deck lives.
pub const HOST: &str = "deck";

/// The name the injected script's message handler is registered under — the
/// JS→Rust channel for "the reader scrolled to another slide".
const MESSAGES: &str = "snslides";

// ---------------------------------------------------------------------------
// The engine and the view
// ---------------------------------------------------------------------------

/// What the scheme handler needs: the deck being served, and the document
/// that presents it.
#[derive(Default)]
pub(crate) struct DeckState {
    deck: RefCell<Option<Arc<SlideDeck>>>,
    /// The whole deck as one HTML document, served at `speechnotes://deck/`
    /// ( the document used to go in through `load_html`, which leaves
    /// the page at an `about:blank` origin — and WebKit never fetches the
    /// page's `speechnotes://` subresources from there, so pictures never
    /// loaded. Served over the scheme like the EPUB viewer's book, the page
    /// origin *is* the deck's and the media is same-origin.)
    html: RefCell<String>,
}

/// The engine the deck's views run on: the scheme handler's host (the
/// state) and the web context the handler is registered on. One per deck
/// viewer. The show's extra views — the audience screen's fullscreen slide,
/// the console's two previews — spawn on this same engine, so every view
/// serves the same deck over the same scheme.
pub(crate) struct Engine {
    state: Rc<DeckState>,
    ctx: webkit6::WebContext,
    /// Called when a **navigating** view reports the reader is on another
    /// slide — a scroll inside the deck, a presentation step, a click on
    /// the audience screen. The reader follows. The console's previews do
    /// not navigate, so they never fire it.
    on_slide: RefCell<Option<Rc<dyn Fn(usize)>>>,
    /// PowerPoint show conventions (2026-11-07): the page's Esc
    /// report — the show's universal exit — comes through here, because
    /// only the app can leave the window-level fill mode the page lives in.
    on_exit_present: RefCell<Option<Rc<dyn Fn()>>>,
}

/// The engine, the deck and where the reader stands — one object, shared by
/// every handle to the viewer.
struct Inner {
    engine: Rc<Engine>,
    /// The web engine's widget. It *is* a GTK widget, so it goes into the
    /// reader's stack like any other.
    view: webkit6::WebView,
    /// Kept so the message handler can be connected after construction (the
    /// view's builder wants the manager first, the handler wants the view).
    manager: webkit6::UserContentManager,
    /// The slide index on screen.
    current: Cell<usize>,
    /// `"width"` | `"page"` | `"none"` — how the deck fills the window.
    /// `"none"` is a manual zoom: the reader zoomed, so resizes must not
    /// re-fit.
    fit_mode: RefCell<String>,
    /// The manual zoom, for when fit mode is `"none"`.
    manual: Cell<f64>,
    /// The size the last fit was computed from, so a resize is a change and
    /// not a per-frame re-fit.
    fitted_size: Cell<(i32, i32)>,
    /// Presentation mode: one slide per screen, click and the navigation
    /// keys step.
    present: Cell<bool>,
    /// Whether a click steps to the next slide while presenting.
    click_advance: Cell<bool>,
    /// A show queued while a load was still running, flushed on finish.
    pending_show: Cell<Option<usize>>,
    /// The commanded positioning still in flight: `(index, armed_at)` from
    /// the last `show_js`. The page reports every settled scroll — including
    /// the commanded one (an echo) and, measured on a document still laying
    /// out, a wrong `show(nearest())` result that read slide 0 and dragged
    /// the reader to the deck's start (mirror, two screens, 2026-11-12).
    /// While this is armed, reports are echoes or noise, not navigation:
    /// `report_allowed` swallows them until the page acknowledges the
    /// commanded index or the grace lapses.
    positioning: Cell<Option<(usize, std::time::Instant)>>,
    /// Whether the script-message handler registered. When it did not, a
    /// poll takes over the slide-follow duty. (Written at construction;
    /// the original lineage reads the local instead of the field.)
    #[allow(dead_code)]
    messages_ok: Cell<bool>,
    /// Speak-from-selection: the page's selection
    /// reports — `(speak_now, Some((slide, byte)))` where byte is the exact
    /// data-s offset the deck's DOM carries, or `None` when the selection
    /// left speakable content. `speak_now` is the bubble's click. The
    /// reader's own view only: a selection made on the audience screen must
    /// not start the voice.
    on_selection: RefCell<Option<Rc<dyn Fn(bool, Option<(usize, usize)>)>>>,
    /// The running show ( 2026-11-12, the PowerPoint-shaped one — the
    /// audience screen's fullscreen slide, the speaker console's rendered
    /// previews), while it is open. Taken and dropped on close, wherever
    /// it ends from: the Present toggle, Esc, the mode leaving, a new deck.
    show: RefCell<Option<Show>>,
    /// The show's step, wired by the reader (its `go_to_chapter`): the
    /// console's buttons and the show windows' arrow keys all take it.
    on_console_step: RefCell<Option<Rc<dyn Fn(i64)>>>,
}

impl Inner {
    /// Run a script in the page. The page takes every command idempotently,
    /// so a lost race with a load is harmless: the load's own handler
    /// re-applies the fit, the present state and the pending show.
    fn eval(&self, script: &str) {
        self.view
            .evaluate_javascript(script, None, None, None::<&gtk4::gio::Cancellable>, |result| {
                if let Err(e) = result {
                    //  was debug-silent in the original; a lost setFit or
                    // show looks exactly like a dead control, so it is worth
                    // a warn on the log.
                    tracing::warn!(error = %e, "the deck page did not take a command");
                }
            });
    }

    /// The resize watcher: gtk4 binds no general `resize` signal for widgets
    /// (GTK 4.8 added one; 0.9's bindings do not carry it), so the fit is
    /// watched the way the render loop itself is: a tick callback comparing
    /// the view's size to the one the last fit was computed from. Two
    /// integer compares per frame; the re-fit fires once per actual change,
    /// so no debounce is needed. Installed on the reader's own view AND on
    /// every view the show spawns — the audience screen's fullscreen
    /// landing is a resize, and the presentation zoom must re-derive from
    /// the projector's real pixels (measured: a slide drawn at the
    /// pre-fullscreen scale, cut at its foot, when only the page's own
    /// resize event stood between them).
    fn install_resize_tick(inner: &Rc<Inner>) {
        let inner = inner.clone();
        let view = inner.view.clone();
        view.add_tick_callback(move |view, _clock| {
            if inner.engine.state.deck.borrow().is_none() {
                return gtk4::glib::ControlFlow::Continue;
            }
            if inner.fit_mode.borrow().as_str() == "none" {
                return gtk4::glib::ControlFlow::Continue;
            }
            let size = (view.width(), view.height());
            if size.0 > 0 && size.1 > 0 && size != inner.fitted_size.get() {
                inner.fitted_size.set(size);
                inner.apply_fit();
                //  a resize while presenting must also re-derive
                // the presentation zoom — the page's own `resize` event
                // has been missed on real hardware, which left a
                // full-screen slide drawn at the pre-resize scale.
                inner.apply_present();
            }
            gtk4::glib::ControlFlow::Continue
        });
    }

    /// Push the current fit into the page.
    ///
    ///  the zoom is computed IN THE PAGE, not here. WebKit's CSS
    /// viewport is the GTK allocation divided by the web process's device
    /// scale — on a fractionally-scaled desktop (measured 1.2) the two
    /// disagree by exactly that factor, and a zoom computed from GTK pixels
    /// laid the slide out wider than its viewport and cut at the right edge.
    /// The page knows its own `innerWidth`; it also derives the device
    /// scale from the GTK size this pushes, so a manual zoom (a GTK-pixel
    /// factor, set by `zoom_step`) still means what the buttons say.
    fn apply_fit(&self) {
        let mode = self.fit_mode.borrow().clone();
        // The index rides along, like setPresent's: during a continuous
        // resize every setFit would otherwise re-read the page's own
        // `nearest()` from a position the PREVIOUS re-fit already drifted,
        // and the drift compounds across the burst. The app's tracked
        // current slide is the one truth that does not move.
        let index = self.current.get() as i64;
        let script = format!(
            "window.__snslides && window.__snslides.setFit({}, {}, {}, {}, {});",
            crate::epubview::json_string(&mode),
            self.view.width(),
            self.view.height(),
            self.manual.get(),
            index
        );
        self.eval(&script);
    }

    /// Push the presentation state into the page (a fresh document has
    /// neither the classes nor the vars of the one before it).
    ///
    /// The resize drift (2026-11-13): the index goes with it. Turning
    /// the presentation on re-lays the page out, and the page's own
    /// `nearest()` read mid-resize has been measured to land the show on the
    /// wrong slide (the mirror run's audience screen read slide 10 for a
    /// show parked on 7) — the Rust-tracked current slide is the truth, and
    /// the page re-lands on it once the geometry settles.
    fn apply_present(&self) {
        let on = self.present.get();
        let click = self.click_advance.get();
        let index = self.current.get() as i64;
        let script = format!(
            "window.__snslides && window.__snslides.setPresent({on}, {click}, {index});"
        );
        self.eval(&script);
    }

    /// Show one slide in the page. Queued for the load handler as well, so
    /// a show asked for while the document was still opening lands when it
    /// has opened.
    fn show_js(&self, index: usize) {
        // Every commanded positioning arms the echo gate (see
        // `report_allowed`): the page's settled reports that follow are
        // echoes of this command until it acknowledges the index — or noise
        // until the grace lapses. User-driven scrolls arm nothing and pass.
        self.positioning
            .set(Some((index, std::time::Instant::now())));
        self.eval(&format!("window.__snslides && window.__snslides.show({index});"));
    }

    /// How long a commanded positioning's echo gate stays armed without the
    /// page's acknowledgement: the load-and-layout window of a big deck on
    /// a cold cache, with margin.
    const ECHO_GRACE: std::time::Duration = std::time::Duration::from_secs(4);

    /// Whether a page report may steer: a commanded positioning (every
    /// `show_js`) makes the page's next settled reports echoes of the
    /// command, not navigation — including a mid-layout
    /// `show(nearest())`'s report, which has been measured to say slide 0
    /// and drag the reader to the deck's start (mirror, two screens,
    /// 2026-11-12). The gate clears on the page's own acknowledgement (a
    /// report of the commanded index) or when the grace lapses; reports
    /// under an armed gate are swallowed either way.
    fn report_allowed(&self, index: usize) -> bool {
        let Some((cmd, armed_at)) = self.positioning.get() else {
            return true; // no commanded positioning in flight: user steering
        };
        if armed_at.elapsed() >= Self::ECHO_GRACE {
            // The grace lapsed without an acknowledgement. The next report
            // is swallowed regardless (the likeliest tail-end noise, or an
            // acknowledgement that is an echo anyway) and the gate is done.
            self.positioning.set(None);
            return false;
        }
        if index == cmd {
            // The acknowledgement: the positioning completed. The report
            // itself is an echo — the reader commanded this index.
            self.positioning.set(None);
            return false;
        }
        // In flight, not the acknowledgement: a report from a document
        // still finding its layout. Swallowed; the gate stays armed.
        false
    }

    /// Everything a finished load needs re-applied: the document was
    /// replaced, so the zoom, the present classes and the queued show all
    /// start from zero.
    ///
    /// The queued show lands FIRST: `setPresent`'s own `show(nearest())`
    /// reads the scroll position, and a document that just opened sits at
    /// slide zero — applying the present state first would report slide 0
    /// to the follow and drag the whole show to the deck's start. Showing
    /// the queued slide first leaves the scroll where the reader asked,
    /// and the present application's `show(nearest())` re-lands on it (the
    /// page's report dedupe drops the repeat).
    fn on_loaded(&self) {
        if let Some(index) = self.pending_show.take() {
            self.show_js(index);
        }
        self.apply_fit();
        self.apply_present();
    }

    /// The poll that replaces the message channel when the registration was
    /// refused: ask the page which slide it is on, four times a second only
    /// while a deck is loaded, and report changes the same way the message
    /// handler would — through the same echo gate (`report_allowed`).
    fn start_poll(inner: &Rc<Inner>) {
        let engine = inner.engine.clone();
        let view = inner.view.clone();
        let inner = inner.clone();
        gtk4::glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
            if engine.state.deck.borrow().is_none() {
                return gtk4::glib::ControlFlow::Continue;
            }
            let engine = engine.clone();
            let inner = inner.clone();
            let view = view.clone();
            view.evaluate_javascript(
                "window.__snslides ? window.__snslides.current() : -1",
                None,
                None,
                None::<&gtk4::gio::Cancellable>,
                move |result| {
                    let Ok(jsc) = result else {
                        return;
                    };
                    let n = jsc.to_str().trim().parse::<f64>().unwrap_or(-1.0);
                    if !(0.0..f64::INFINITY).contains(&n) {
                        return;
                    }
                    let index = n as usize;
                    if !inner.report_allowed(index) {
                        return;
                    }
                    let callback = engine.on_slide.borrow().clone();
                    if let Some(callback) = callback {
                        callback(index);
                    }
                },
            );
            gtk4::glib::ControlFlow::Continue
        });
    }
}

/// A web view on a deck engine: the settings fence, the script injection
/// and the message channel, built the same for the reader's own view and
/// for every view the show spawns. Returns the view, its manager (the
/// handler connects after the view exists) and whether the message channel
/// registered.
fn make_view(ctx: &webkit6::WebContext) -> (webkit6::WebView, webkit6::UserContentManager, bool) {
    let settings = webkit6::Settings::new();
    settings.set_enable_javascript(true);
    settings.set_enable_developer_extras(false);
    settings.set_enable_write_console_messages_to_stdout(false);
    // A deck's page is served from this app's own scheme and has no
    // links of its own: no DNS prefetch, no file access, nothing to
    // phone. The same fence the book page sits behind.
    settings.set_enable_dns_prefetching(false);
    settings.set_allow_file_access_from_file_urls(false);
    settings.set_allow_universal_access_from_file_urls(false);

    // The slide script, injected at document start: it defines the
    // helpers (show, fit, present, highlight) before the page renders,
    // so a deck never flashes unstyled.
    let manager = webkit6::UserContentManager::new();
    manager.add_script(&webkit6::UserScript::new(
        SLIDES_SCRIPT,
        webkit6::UserContentInjectedFrames::TopFrame,
        webkit6::UserScriptInjectionTime::Start,
        &[],
        &[],
    ));
    // The JS→Rust channel: the page reports slide changes as they
    // happen. When the registration is refused, a poll covers the same
    // duty.
    let messages_ok = manager.register_script_message_handler(MESSAGES, None);

    let view = webkit6::WebView::builder()
        .web_context(ctx)
        .settings(&settings)
        .user_content_manager(&manager)
        .build();
    view.set_css_classes(&["slide-view"]);
    (view, manager, messages_ok)
}

/// The slide deck's page in the reader: a web engine showing the deck.
///
/// One webview for the whole deck — every slide is in the one document — so
/// stepping through is scrolling, not loading.
#[derive(Clone)]
pub struct SlideView {
    inner: Rc<Inner>,
}

impl SlideView {
    /// A web engine with the deck's scheme registered and the slide script
    /// in place.
    pub fn new() -> Self {
        let state = Rc::new(DeckState::default());
        let ctx = webkit6::WebContext::new();

        // The handler: the only way into the deck's media.
        let served = state.clone();
        ctx.register_uri_scheme(SCHEME, move |req| {
            serve_request(&served, req);
        });
        // A local scheme, as for the book: the document's own images resolve
        // without a cross-origin refusal.
        if let Some(manager) = ctx.security_manager() {
            manager.register_uri_scheme_as_local(SCHEME);
        }

        let (view, manager, messages_ok) = make_view(&ctx);
        let engine = Rc::new(Engine {
            state,
            ctx,
            on_slide: RefCell::new(None),
            on_exit_present: RefCell::new(None),
        });

        let inner = Rc::new(Inner {
            engine,
            view,
            manager,
            current: Cell::new(0),
            fit_mode: RefCell::new("width".to_string()),
            manual: Cell::new(1.0),
            fitted_size: Cell::new((0, 0)),
            present: Cell::new(false),
            click_advance: Cell::new(true),
            pending_show: Cell::new(None),
            positioning: Cell::new(None),
            messages_ok: Cell::new(messages_ok),
            on_selection: RefCell::new(None),
            show: RefCell::new(None),
            on_console_step: RefCell::new(None),
        });
        Self::install_handlers(&inner);
        Self { inner }
    }

    /// The finished-load work, shared by the reader's own view and every
    /// show view: the queued show flushes, the fit and the present state
    /// re-apply — and the queued position re-asserts.
    ///
    ///  the re-assert exists because a load that has
    /// finished is not a layout that has settled. Measured on the audience
    /// screen (mirror, two screens): the flush's scrollIntoView landed
    /// SHORT and the projector sat on a slide it was not sent to until the
    /// speaker's first step moved it. The page takes `show` idempotently,
    /// and the echo gate (`report_allowed`) keeps every re-assert's report
    /// from steering anything, so re-asserting is always safe.
    fn finish_load(inner: &Rc<Inner>) {
        let queued = inner.pending_show.get();
        inner.on_loaded();
        for delay in [400, 1200] {
            let Some(index) = queued else {
                continue;
            };
            let inner = inner.clone();
            gtk4::glib::timeout_add_local(std::time::Duration::from_millis(delay), move || {
                inner.show_js(index);
                gtk4::glib::ControlFlow::Break
            });
        }
    }

    /// The web engine's widget, for the reader's stack.
    pub fn widget(&self) -> &webkit6::WebView {
        &self.inner.view
    }

    /// The load, resize and navigation handlers.
    fn install_handlers(inner: &Rc<Inner>) {
        // Every finished load re-applies everything the document carries:
        // the fit, the present state and any show queued mid-load.
        {
            let inner = inner.clone();
            let view = inner.view.clone();
            view.connect_load_changed(move |_, event| {
                if event == webkit6::LoadEvent::Finished {
                    Self::finish_load(&inner);
                }
            });
        }

        // The page's slide reports: the reader's place follows the scroll.
        // Connected here, after `Inner` exists — the manager had to go into
        // the view's builder first.
        {
            let inner = inner.clone();
            let manager = inner.manager.clone();
            manager
                .connect_script_message_received(Some(MESSAGES), move |_, value| {
                    let msg = value.to_str().to_string();
                    if let Some(rest) = msg.strip_prefix("slidechanged:") {
                        if let Ok(index) = rest.trim().parse::<usize>() {
                            if inner.report_allowed(index) {
                                inner.current.set(index);
                                let callback = inner.engine.on_slide.borrow().clone();
                                if let Some(callback) = callback {
                                    callback(index);
                                }
                            }
                        }
                    }
                    // Speak-from-selection: the
                    // selection reports — `speak:<slide>:<byte>` from the
                    // bubble's click, `sel:<slide>:<byte>` as a selection
                    // settles, `-1:-1` when it leaves speakable content.
                    //  `esc:` — the page's Esc while
                    // presenting. The show's exit belongs to the app.
                    if msg == "esc:0:0" {
                        let callback = inner.engine.on_exit_present.borrow().clone();
                        if let Some(callback) = callback {
                            callback();
                        }
                        return;
                    }
                    let report = if let Some(rest) = msg.strip_prefix("speak:") {
                        parse_sel_report(rest).map(|pos| (true, Some(pos)))
                    } else if let Some(rest) = msg.strip_prefix("sel:") {
                        Some((false, parse_sel_report(rest)))
                    } else {
                        None
                    };
                    if let Some((speak_now, pos)) = report {
                        let callback = inner.on_selection.borrow().clone();
                        if let Some(callback) = callback {
                            callback(speak_now, pos);
                        }
                    }
                });
        }

        // A resize changes what "fit" means: the watcher below.
        Inner::install_resize_tick(inner);

        // A deck has no links; anything that is not this scheme is either
        // handed to the desktop (the http/mailto the family allows) or
        // refused — the same fence the book page sits behind.
        inner.view.connect_decide_policy(|_, decision, decision_type| {
            if decision_type != webkit6::PolicyDecisionType::NavigationAction {
                return false;
            }
            let Some(navigation) = decision.downcast_ref::<webkit6::NavigationPolicyDecision>()
            else {
                return false;
            };
            let Some(mut action) = navigation.navigation_action() else {
                return false;
            };
            let Some(request) = action.request() else {
                return false;
            };
            let Some(uri) = request.uri() else {
                return false;
            };
            let uri = uri.to_string();
            //  load_html's own initial navigation *is* about:blank
            // (the deck is loaded as a string, no base URI). Refusing it
            // refused every load — the deck page never appeared. Let the
            // engine's own blank through; the deck's media still goes only
            // to the scheme below.
            if uri == "about:blank" {
                return false;
            }
            if uri.starts_with(&format!("{SCHEME}://")) {
                return false; // the deck's own media
            }
            if crate::epubview::is_safe_link(&uri) {
                gtk4::UriLauncher::new(&uri).launch(
                    gtk4::Window::NONE,
                    gtk4::gio::Cancellable::NONE,
                    |result| {
                        if let Err(e) = result {
                            tracing::warn!(error = %e, "the desktop refused to open a link");
                        }
                    },
                );
                decision.ignore();
                return true;
            }
            tracing::warn!(uri = %uri, "refusing to follow a link with an unlisted scheme");
            decision.ignore();
            true
        });
    }

    /// Another view on this deck's engine: the same scheme handler, the
    /// same deck document, its own script channel. `interactive` views —
    /// the audience screen's — report their navigation through the shared
    /// follow, so a click or a key on the audience screen steers the show;
    /// the console's previews are non-interactive: they render, they do
    /// not steer.
    pub(crate) fn spawn_show_view(&self, interactive: bool) -> SlideView {
        let (view, manager, messages_ok) = make_view(&self.inner.engine.ctx);
        let inner = Rc::new(Inner {
            engine: self.inner.engine.clone(),
            view,
            manager,
            current: Cell::new(0),
            fit_mode: RefCell::new("page".to_string()),
            manual: Cell::new(1.0),
            fitted_size: Cell::new((0, 0)),
            present: Cell::new(false),
            click_advance: Cell::new(false),
            pending_show: Cell::new(None),
            positioning: Cell::new(None),
            messages_ok: Cell::new(messages_ok),
            on_selection: RefCell::new(None),
            show: RefCell::new(None),
            on_console_step: RefCell::new(None),
        });
        // The load + message handlers, without the sibling's own navigation
        // policy (a show view has no links of its own and its media is the
        // same scheme — the reader's fence does not need repeating here).
        {
            let inner = inner.clone();
            let view = inner.view.clone();
            view.connect_load_changed(move |_, event| {
                if event == webkit6::LoadEvent::Finished {
                    Self::finish_load(&inner);
                }
            });
        }
        Inner::install_resize_tick(&inner);
        if interactive {
            let inner = inner.clone();
            let manager = inner.manager.clone();
            manager.connect_script_message_received(Some(MESSAGES), move |_, value| {
                let msg = value.to_str().to_string();
                if let Some(rest) = msg.strip_prefix("slidechanged:") {
                    if let Ok(index) = rest.trim().parse::<usize>() {
                        // The same echo gate the reader's own view answers
                        // to: a commanded positioning's reports are echoes
                        // and load noise, not steering — the audience's
                        // load-time `show(nearest())` has been measured to
                        // say slide 0 and drag the whole show to the deck's
                        // start. A click or a key on this screen reports
                        // outside any commanded positioning and passes.
                        if inner.report_allowed(index) {
                            inner.current.set(index);
                            let callback = inner.engine.on_slide.borrow().clone();
                            if let Some(callback) = callback {
                                callback(index);
                            }
                        }
                    }
                }
                // The audience page's Esc ends the show like the reader's
                // own does.
                if msg == "esc:0:0" {
                    let callback = inner.engine.on_exit_present.borrow().clone();
                    if let Some(callback) = callback {
                        callback();
                    }
                }
            });
        }
        if !messages_ok && interactive {
            Inner::start_poll(&inner);
        }
        Self { inner }
    }

    /// Load the deck document into this view. The reader's own view calls
    /// it through `set_deck`; the show's spawned views call it directly —
    /// the engine's state already holds the deck (it was set when the
    /// reader opened it), so a sibling load must not rewrite it.
    pub(crate) fn load_deck(&self) {
        self.inner
            .view
            .load_uri(&format!("{SCHEME}://{HOST}/deck.html"));
    }

    /// Set the deck being shown. The whole deck becomes one document; the
    /// show and the fit follow on the load handler.
    pub fn set_deck(&self, deck: Arc<SlideDeck>) {
        // A new document invalidates the show's windows: close them rather
        // than leave them describing the deck that just went away. (The
        // show is per-deck state.)
        self.end_show();
        *self.inner.engine.state.deck.borrow_mut() = Some(deck.clone());
        self.inner.current.set(0);
        self.inner.pending_show.set(None);
        *self.inner.engine.state.html.borrow_mut() = deck_html(&deck);
        self.load_deck();
    }

    /// Scroll the deck to slide `index`.
    pub fn show_slide(&self, index: usize) {
        self.inner.current.set(index);
        self.inner.pending_show.set(Some(index));
        self.inner.show_js(index);
    }

    /// The slide index on screen.
    pub fn current(&self) -> usize {
        self.inner.current.get()
    }

    /// Set the callback that fires when the page reports a slide change.
    pub fn on_slide(&self, callback: impl Fn(usize) + 'static) {
        *self.inner.engine.on_slide.borrow_mut() = Some(Rc::new(callback));
    }

    /// Speak-from-selection: set the callback the
    /// page's selection reports go through.
    pub fn on_selection(
        &self,
        callback: impl Fn(bool, Option<(usize, usize)>) + 'static,
    ) {
        *self.inner.on_selection.borrow_mut() = Some(Rc::new(callback));
    }

    /// PowerPoint show conventions (2026-11-07): set the callback the
    /// page's Esc (end the show) reports go through.
    pub fn on_exit_present(&self, callback: impl Fn() + 'static) {
        *self.inner.engine.on_exit_present.borrow_mut() = Some(Rc::new(callback));
    }

    /// The show's step, wired by the reader: the console's buttons and the
    /// show windows' arrow keys take it (a negative step goes back).
    pub fn on_console_step(&self, callback: impl Fn(i64) + 'static) {
        *self.inner.on_console_step.borrow_mut() = Some(Rc::new(callback));
    }

    /// Highlight the speakable range `[start, end)` — **bytes** of the
    /// slide's text, the plan-chunk domain — on the slide itself.
    pub fn highlight(&self, start: usize, end: usize) {
        let script = format!(
            "window.__snslides && window.__snslides.highlight({start},{end});"
        );
        self.inner.eval(&script);
    }

    /// Remove the slide's read-along highlight.
    pub fn clear_highlight(&self) {
        self.inner
            .eval("window.__snslides && window.__snslides.clearHighlight();");
    }

    /// Presentation mode: one slide per screen, scroll-snapped, click and
    /// the navigation keys stepping. The reader's focus mode *is* this, for
    /// a deck.
    pub fn set_present(&self, on: bool, click_advance: bool) {
        self.inner.present.set(on);
        self.inner.click_advance.set(click_advance);
        self.inner.apply_present();
    }

    /// How the deck fills the window: `"width"`, `"page"` or `"none"` (a
    /// manual zoom). Applied now, and the load handler re-applies it.
    pub fn set_fit(&self, mode: &str) {
        *self.inner.fit_mode.borrow_mut() = mode.to_string();
        self.inner.apply_fit();
    }

    /// Step the zoom when fit mode is off: ×1.25 per step, the reader's
    /// gesture. A step out of a fit mode lands at 1× first, so the buttons
    /// always start from somewhere known.
    pub fn zoom_step(&self, delta: i32) {
        if self.inner.fit_mode.borrow().as_str() != "none" {
            self.inner.manual.set(1.0);
        }
        let mut zoom = self.inner.manual.get();
        if delta > 0 {
            zoom *= 1.25;
        } else {
            zoom /= 1.25;
        }
        let zoom = zoom.clamp(0.2, 5.0);
        self.inner.manual.set(zoom);
        *self.inner.fit_mode.borrow_mut() = "none".to_string();
        self.inner.apply_fit();
    }

    /// The surround the deck sits on: the reader theme's own colour. The
    /// slides keep their colours — a slide is a fixed-size document, and
    /// retheming one is not preserving it — but the page around them follows
    /// the app, so a dark reader does not stare at a white wall.
    pub fn set_theme(&self, theme: &str) {
        let scheme = crate::epubview::scheme_for(theme);
        let rgba =
            gtk4::gdk::RGBA::parse(scheme.background).unwrap_or(gtk4::gdk::RGBA::WHITE);
        self.inner.view.set_background_color(&rgba);
    }
}

/// The fit zoom: CSS pixels per point for the mode.
///
/// `width` fills the width; `page` also fits the height, so a whole slide is
/// visible. The slack is the margin around the slides (and their shadow), so
/// "fit" means fit, not "overflow by exactly one scrollbar".
///
/// Since alpha-3 the production fit is computed in the page's own JavaScript
/// (device-scale aware); this Rust copy survives as the reference the zoom
/// tests assert against, so it is compiled for tests only.
#[cfg(test)]
fn zoom_for(mode: &str, vw: f64, vh: f64, deck_w: f64, deck_h: f64) -> f64 {
    const PX_PER_PT: f64 = 4.0 / 3.0;
    let avail_w = (vw - 16.0).max(120.0);
    let by_width = avail_w / (deck_w.max(1.0) * PX_PER_PT);
    match mode {
        "page" => {
            let avail_h = (vh - 36.0).max(90.0);
            by_width.min(avail_h / (deck_h.max(1.0) * PX_PER_PT)).max(0.05)
        }
        _ => by_width.clamp(0.05, 8.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // The text helpers live in document.rs as private fns; the tests reach
    // them directly.
    use super::document::{escape_html, num};
    use sn_doc::slides::{Anchor, Content, Item};

    //  this test was original self-contradictory —
    // it asserted `page < width` *and* `page == 764/540 ≈ 1.415` for a
    // geometry where width-fit is 984/960 ≈ 1.025. Both cannot hold. Fit-page
    // is min(width-fit, height-fit) — `zoom_for` implements exactly that —
    // because a page zoomed to 1.415 here would be 1358 CSS px wide inside a
    // 984 px viewport: "fit page" that overflows the page. The old test's
    // arithmetic was wrong ("1.41, which is smaller" — smaller than 1.025 it
    // is not); the asserts now pin the actual, correct semantics, with a
    // second geometry where the height genuinely binds.
    #[test]
    fn the_fit_zoom_fills_what_the_mode_names() {
        // A 720×405 pt deck is 960×540 CSS px at scale 1.
        let width = zoom_for("width", 1000.0, 800.0, 720.0, 405.0);
        let page = zoom_for("page", 1000.0, 800.0, 720.0, 405.0);
        // Width fit ignores the height slack. Page fit is the smaller of the
        // two constraints; here the width binds (764/540 ≈ 1.41 would
        // overflow the viewport sideways), so page fit equals width fit.
        assert!((width - (984.0 / 960.0)).abs() < 1e-9);
        assert!((page - width).abs() < 1e-9);
        // A short viewport makes the height bind instead: page fit stops at
        // what the height allows and never exceeds the width fit.
        let page_short = zoom_for("page", 1000.0, 500.0, 720.0, 405.0);
        assert!(page_short < width);
        assert!((page_short - (464.0 / 540.0)).abs() < 1e-9);
    }

    #[test]
    fn a_degenerate_deck_never_divides_by_zero() {
        assert!(zoom_for("page", 100.0, 100.0, 0.0, 0.0).is_finite());
        assert!(zoom_for("width", 0.0, 0.0, 720.0, 405.0).is_finite());
    }

    #[test]
    fn numbers_read_like_css_numbers() {
        assert_eq!(num(720.0), "720");
        assert_eq!(num(12.50), "12.5");
        assert_eq!(num(-0.0), "0");
        assert_eq!(num(0.0), "0");
    }

    #[test]
    fn html_is_escaped_in_text_and_attributes() {
        assert_eq!(escape_html("a<b>&\"'"), "a&lt;b&gt;&amp;&quot;&#39;");
        assert_eq!(escape_html("plain text"), "plain text");
    }

    #[test]
    fn a_cropped_picture_is_inset_by_its_crop() {
        let deck = SlideDeck {
            w: 720.0,
            h: 405.0,
            media: vec![sn_doc::slides::DeckMedia {
                name: "m1".to_string(),
                content_type: "image/png".to_string(),
                data: Arc::new(vec![0u8]),
            }],
            ..Default::default()
        };
        let item = Item {
            x: 0.0,
            y: 0.0,
            w: 400.0,
            h: 300.0,
            rot: 0.0,
            flip_h: false,
            flip_v: false,
            fill: None,
            border: None,
            radius: 0.0,
            svg: None,
            shadow: None,
            content: Content::Text {
                paras: Vec::new(),
                anchor: Anchor::Top,
                wrap: true,
                dir: String::new(),
            },
        };
        // A 25% crop off the left: the image is 4/3 wider than the frame and
        // slid left by 100pt.
        let html = picture_html(&deck, 0, &[0.25, 0.0, 0.0, 0.0], &item);
        assert!(html.contains("left:-100pt"));
        assert!(html.contains("width:533.33pt"));
        // No crop: the plain filling image.
        let plain = picture_html(&deck, 0, &[0.0, 0.0, 0.0, 0.0], &item);
        assert!(plain.contains("class=\"pic\""));
        assert!(!plain.contains("picwrap"));
        // An out-of-range media index renders nothing rather than a 404ing URL.
        assert!(picture_html(&deck, 9, &[0.0, 0.0, 0.0, 0.0], &item).is_empty());
    }
}
