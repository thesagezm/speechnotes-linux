//! The EPUB viewer: **the book as the book**, in a real web engine.
//!
//! This is calibre's model, copied deliberately:
//!
//! * The book's spine item is loaded into a web engine **as it is** — the
//!   publisher's own XHTML, with the publisher's own `<link rel=stylesheet>`
//!   resolving against it. A GTK label cannot carry a stylesheet, which is
//!   why the old reader lost the design: First Aid's blue chapter banners,
//!   its tinted clinical panels, its ■ bullets and its letter-spaced
//!   headings were all CSS.
//! * The bytes are served over a private URL scheme
//!   (`speechnotes://book/<zip path>`), never from the filesystem. The
//!   scheme handler is the only way into the archive, so a book's `href`
//!   cannot escape it — the same discipline calibre's `calibre://` handler
//!   uses.
//! * The reader's appearance is a **CSS layer on top of the book**, never a
//!   re-render of it. Calibre's `override_book_colors` tri-state is the
//!   preserve-or-retheme switch; our "respect the book's styles" toggle is
//!   the same escape hatch, and it is **on** by default.
//!
//! The split the reader asks for falls out of this: the **webview** is the
//! book (what you look at), and the **parsed text** (`sn_doc::chapter`)
//! feeds the voice and the read-along view. The parsed text is computed and
//! kept; it is not what you see.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use gtk4::prelude::*;
use webkit6::prelude::*;

use sn_doc::zip::Archive;

/// The URL scheme the book's bytes are served over.
pub const SCHEME: &str = "speechnotes";

/// Speak-from-selection: the name the page's
/// selection reports go through — `sel:<text>` as a selection settles,
/// `speak:<text>` when its "Speak from here" bubble is clicked.
const EPUB_MESSAGES: &str = "snebook";

/// The host under which the book lives. One host, one book, one archive.
pub const HOST: &str = "book";

// ---------------------------------------------------------------------------
// Appearance
// ---------------------------------------------------------------------------

/// The reader's appearance over the book: calibre's knobs, our names.
///
/// Every field here has a default that means *do not touch the book*: the
/// margins are the space around the text, the font size is the base the
/// book's own `em` sizes scale from, and the colour override is off. A
/// reader that changes the book's design by default is not preserving it.
#[derive(Debug, Clone, PartialEq)]
pub struct EpubAppearance {
    /// `"light"` | `"sepia"` | `"dark"` | `"trueBlack"`.
    pub theme: String,
    /// `""` (the book's own) | `"serif"` | `"sans"` | `"mono"`.
    pub font: String,
    /// A family name, overriding `font`. Empty = the engine's default.
    pub font_family: String,
    /// The base font size, in pixels.
    pub font_size: f64,
    /// The smallest font the engine will use, in pixels.
    pub min_font_size: f64,
    /// Extra space between letters, in pixels.
    pub letter_spacing: f64,
    /// The line height, as a multiplier. Only applied when the book's own
    /// styles are *not* respected.
    pub line_height: f64,
    /// The space after a paragraph, in pixels. Same condition.
    pub paragraph_spacing: f64,
    pub margin_top: f64,
    pub margin_bottom: f64,
    pub margin_left: f64,
    pub margin_right: f64,
    /// The widest the text column may be. Zero = as wide as the window.
    pub max_width: f64,
    /// `"flow"` (a scroll) | `"paged"` (one screen at a time).
    pub flow: String,
    /// How many columns a paged spread has.
    pub columns: u32,
    /// `"never"` | `"dark"` | `"always"` — whether the scheme's colours
    /// replace the book's own. Calibre's `override_book_colors`.
    pub override_colors: String,
    /// Whether the book's own stylesheet wins. On by default.
    pub respect_styles: bool,
}

impl Default for EpubAppearance {
    fn default() -> Self {
        Self {
            theme: "light".to_string(),
            font: String::new(),
            font_family: String::new(),
            font_size: 18.0,
            min_font_size: 10.0,
            letter_spacing: 0.0,
            line_height: 1.55,
            paragraph_spacing: 10.0,
            margin_top: 16.0,
            margin_bottom: 16.0,
            margin_left: 24.0,
            margin_right: 24.0,
            max_width: 0.0,
            flow: "flow".to_string(),
            columns: 1,
            override_colors: "never".to_string(),
            respect_styles: true,
        }
    }
}

/// A colour scheme: the foreground, the background and the link colour.
///
/// Calibre's five schemes, folded into the four the family shares
/// (`light`, `sepia`, `dark`, `trueBlack`). The "system" scheme is the
/// app's own light/dark choice, not a fifth colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scheme {
    pub foreground: &'static str,
    pub background: &'static str,
    pub link: &'static str,
    pub dark: bool,
}

/// The scheme a theme name means.
pub fn scheme_for(theme: &str) -> Scheme {
    match theme {
        "sepia" => Scheme {
            foreground: "#39322B",
            background: "#F6F3E9",
            link: "#8A5A20",
            dark: false,
        },
        "dark" => Scheme {
            foreground: "#E6E4E2",
            background: "#1C1C1E",
            link: "#7AA2F7",
            dark: true,
        },
        "trueBlack" => Scheme {
            foreground: "#FFFFFF",
            background: "#000000",
            link: "#7AA2F7",
            dark: true,
        },
        _ => Scheme {
            foreground: "#000000",
            background: "#FFFFFF",
            link: "#1A5FB4",
            dark: false,
        },
    }
}

// ---------------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------------

/// What the scheme handler serves from. An EPUB is its archive — the
/// book's own files, read as they lie. The headless documents (the
/// Mobipocket/Kindle family, FB2, RTF, HTML, plain text) have no container,
/// so the parsed book is what is served: the spine names the parse's
/// chapters and `chapter-N` answers with the page the parse synthesises.
enum BookSource {
    Zip(Arc<Archive>),
    Parts(Arc<sn_store::books::ServedBookDoc>),
}

impl BookSource {
    /// The bytes for one virtual path, or `None` when neither the archive
    /// nor the parse holds it — the handler's 404, never a guess.
    fn read(&self, name: &str) -> Option<Vec<u8>> {
        match self {
            BookSource::Zip(archive) => archive.read(name).ok(),
            BookSource::Parts(parts) => parts.read(name),
        }
    }
}

/// What the scheme handler needs: the book being served, and the spine so
/// a request can be answered with the spine index it names.
#[derive(Default)]
struct BookState {
    source: RefCell<Option<BookSource>>,
    spine: RefCell<Vec<String>>,
}

/// The EPUB's page in the reader: a web engine showing the book.
///
/// One webview is reused across the whole book: a webview per chapter
/// spawns a web process per chapter, and a 90-item spine would leave 90
/// processes behind.
pub struct EpubView {
    /// The web engine's widget. It *is* a GTK widget, so it goes into the
    /// reader's stack like any other.
    pub view: webkit6::WebView,
    /// The engine's settings, kept so a font change can be applied without
    /// rebuilding the view.
    settings: webkit6::Settings,
    state: Rc<BookState>,
    /// The appearance last applied.
    // Handle-sharing fix: the four
    // mutable fields below used to be bare RefCell/Cell members, and the
    // manual `Clone` impl copied them into each clone. That contradicted the
    // impl's own doc ("a clone is a handle, not a copy") and broke every
    // callback: the load-changed handler, the script-message handler and the
    // appearance applier each captured their own clone at connect time, so
    // they read RefCells the later `on_load`/`on_selection`/`set_appearance`
    // setters never touched — messages arrived and were dropped on the floor
    // (the speak-from-selection EPUB path never started a session; in-book
    // link navigation and appearance-on-reload were equally dead). They are
    // `Rc`-shared now, which is what the doc always promised.
    appearance: Rc<RefCell<EpubAppearance>>,
    /// The spine index on screen.
    current: Rc<Cell<usize>>,
    /// Called after every finished load with the spine index the page is
    /// (or `None` when the page is not a spine item). The reader uses it to
    /// notice a chapter change caused by a link inside the book.
    on_load: Rc<RefCell<Option<Rc<dyn Fn(Option<usize>)>>>>,
    /// Speak-from-selection: the page's selection
    /// reports — `(speak_now, text)`. `speak_now` is the "Speak from here"
    /// bubble's click; otherwise the report only arms F5.
    on_selection: Rc<RefCell<Option<Rc<dyn Fn(bool, String)>>>>,
    /// Fixed 2026-11-01: the page's scroll position as the
    /// page last reported it — `(y, max)`, or `None` before the first
    /// report of the current page. The webview is not a `Scrollable`, so
    /// the reader's keyboard scrolls the page through the page itself, and
    /// this is how it knows whether the page can still move (scroll) or is
    /// at its edge (turn the chapter).
    scroll_state: Rc<Cell<Option<(f64, f64)>>>,
    /// The web engine's factory font settings, read once at construction.
    /// "Respect the book's styles" means the book's own design rules — for
    /// a page the book styles no further than the engine's defaults, and
    /// restoring these is what makes the switch a *revert*: a base size or
    /// a family the reader pushed while the toggle was off must not leak
    /// into a book the reader is now told to respect.
    engine_default_family: String,
    engine_default_size: u32,
    engine_default_min_size: u32,
}

impl EpubView {
    /// The web engine's widget, for the reader's stack and the focus walk.
    //  authored from the call sites (reader.rs `epub.widget()`;
    // slideview.rs carries the same accessor); the final epubview.rs body
    // carrying it did not survive.
    pub fn widget(&self) -> &webkit6::WebView {
        &self.view
    }

    /// A web engine with the book's scheme registered and the reader's
    /// script in place.
    pub fn new() -> Self {
        let state = Rc::new(BookState::default());
        let ctx = webkit6::WebContext::new();

        // The handler: the only way into the archive.
        let served = state.clone();
        ctx.register_uri_scheme(SCHEME, move |req| {
            serve_request(&served, req);
        });
        // A local scheme: the document may read the files it references
        // (its stylesheet, its images) without a cross-origin refusal.
        if let Some(manager) = ctx.security_manager() {
            manager.register_uri_scheme_as_local(SCHEME);
        }

        let settings = webkit6::Settings::new();
        settings.set_enable_javascript(true);
        settings.set_enable_developer_extras(false);
        settings.set_enable_write_console_messages_to_stdout(false);

        // The reader's script: it defines the helpers the appearance is
        // applied with, on every document, before the page's own scripts
        // run — so a chapter never flashes unstyled.
        let manager = webkit6::UserContentManager::new();
        manager.add_script(&webkit6::UserScript::new(
            VIEWER_SCRIPT,
            webkit6::UserContentInjectedFrames::TopFrame,
            webkit6::UserScriptInjectionTime::Start,
            &[],
            &[],
        ));
        // Speak-from-selection: the JS→Rust channel
        // the page's selection reports go through.
        let selections_ok = manager.register_script_message_handler(EPUB_MESSAGES, None);
        //  the original built a WebView here and then built the real one
        // again below with the user-content manager, leaving the first as a
        // discarded construction. Collapsed 2026-10-30 (beta perf review) to
        // the single manager-bearing build — identical widget, one web engine
        // object instead of two.
        let view = webkit6::WebView::builder()
            .web_context(&ctx)
            .settings(&settings)
            .user_content_manager(&manager)
            .build();
        view.set_css_classes(&["epub-view"]);

        let epub = Self {
            view,
            state,
            appearance: Rc::new(RefCell::new(EpubAppearance::default())),
            current: Rc::new(Cell::new(0)),
            on_load: Rc::new(RefCell::new(None)),
            on_selection: Rc::new(RefCell::new(None)),
            scroll_state: Rc::new(Cell::new(None)),
            engine_default_family: settings
                .default_font_family()
                .unwrap_or_default()
                .to_string(),
            engine_default_size: settings.default_font_size(),
            engine_default_min_size: settings.minimum_font_size(),
            settings,
        };
        epub.install_handlers();
        // Speak-from-selection: the page's selection
        // reports land here, forwarded to the reader's handler.
        if selections_ok {
            let epub = epub.clone();
            manager.connect_script_message_received(Some(EPUB_MESSAGES), move |_, value| {
                let msg = value.to_str().to_string();
                // Fixed 2026-11-01: the page's scroll report
                // — `scroll:<y>:<max>` — is the keyboard's scroll-or-turn
                // evidence; it is consumed here, not forwarded.
                if let Some(rest) = msg.strip_prefix("scroll:") {
                    if let Some((y, max)) = rest.split_once(':') {
                        if let (Ok(y), Ok(max)) = (y.parse::<f64>(), max.parse::<f64>()) {
                            epub.scroll_state.set(Some((y, max)));
                        }
                    }
                    return;
                }
                let report = if let Some(rest) = msg.strip_prefix("speak:") {
                    Some((true, rest.to_string()))
                } else if let Some(rest) = msg.strip_prefix("sel:") {
                    Some((false, rest.to_string()))
                } else {
                    None
                };
                if let Some((speak_now, text)) = report {
                    if let Some(callback) = epub.on_selection.borrow().as_ref() {
                        callback(speak_now, text);
                    }
                }
            });
        }
        epub
    }

    /// The load and navigation handlers.
    fn install_handlers(&self) {
        // Every finished load re-applies the appearance: the document was
        // replaced, so the layer the previous one carried is gone.
        let weak = self.clone();
        self.view.connect_load_changed(move |view, event| {
            if event != webkit6::LoadEvent::Finished {
                return;
            }
            // A new document's scroll position is not the old one's: drop
            // the last report and let the fresh page send its own.
            weak.scroll_state.set(None);
            let index = weak.index_of_uri(view.uri().as_deref());
            weak.current.set(index.unwrap_or(weak.current.get()));
            weak.apply_to_page();
            if let Some(callback) = weak.on_load.borrow().as_ref() {
                callback(index);
            }
        });

        // A link out of the book opens in the user's browser; a link
        // inside the book stays in the book.
        self.view.connect_decide_policy(|_, decision, decision_type| {
            if decision_type != webkit6::PolicyDecisionType::NavigationAction {
                return false;
            }
            let Some(navigation) = decision.downcast_ref::<webkit6::NavigationPolicyDecision>()
            else {
                return false;
            };
            //  webkit6 0.4's PolicyAction::request takes &mut self.
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
            if uri.starts_with(&format!("{SCHEME}://")) {
                return false; // the book's own page: the webview handles it
            }
            if is_safe_link(&uri) {
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
            //  the engine's own blank
            // goes through — reader::prewarm starts the web process with a
            // throwaway about:blank load, and refusing it here refused the
            // prewarm (slideview's decide-policy has carried the same fence
            // since its load_html days). The book's media still goes only
            // to the scheme below.
            if uri == "about:blank" {
                return false;
            }
            // Anything else — `file:`, `javascript:`, a custom scheme — is
            // refused: a book must not be able to run something on the
            // machine that opened it.
            tracing::warn!(uri = %uri, "refusing to follow a link with an unlisted scheme");
            decision.ignore();
            true
        });
    }

    /// Set the book being shown. The archive is the whole book in memory;
    /// the spine is its reading order.
    pub fn set_book(&self, archive: Arc<Archive>, spine: Vec<String>) {
        *self.state.source.borrow_mut() = Some(BookSource::Zip(archive));
        *self.state.spine.borrow_mut() = spine;
    }

    /// Set a parsed document being shown — the Mobipocket/Kindle family,
    /// FB2, RTF, HTML or plain text. The spine names the parse's chapters
    /// and the scheme handler serves them out of it; the appearance layer
    /// dresses the pages exactly as it dresses a book's own.
    pub fn set_parts(&self, parts: Arc<sn_store::books::ServedBookDoc>, spine: Vec<String>) {
        *self.state.source.borrow_mut() = Some(BookSource::Parts(parts));
        *self.state.spine.borrow_mut() = spine;
    }

    /// The spine index a URI names, if it names one.
    fn index_of_uri(&self, uri: Option<&str>) -> Option<usize> {
        let uri = uri?;
        let path = uri.split_once("://")?.1;
        let path = path.split_once('/')?.1;
        let path = sn_doc::epub::percent_decode(path);
        self.state
            .spine
            .borrow()
            .iter()
            .position(|entry| *entry == path)
    }

    /// Show one spine item. `fragment` scrolls to an anchor inside it once
    /// the page has loaded.
    pub fn load_spine_item(&self, index: usize, fragment: Option<&str>) {
        let spine = self.state.spine.borrow();
        let Some(path) = spine.get(index) else {
            return;
        };
        self.current.set(index);
        let mut uri = format!("{SCHEME}://{HOST}/{}", encode_path(path));
        if let Some(fragment) = fragment.filter(|f| !f.is_empty()) {
            uri.push('#');
            uri.push_str(&encode_path(fragment));
        }
        drop(spine);
        self.view.load_uri(&uri);
    }

    /// The spine index on screen.
    pub fn current(&self) -> usize {
        self.current.get()
    }

    /// Set the callback that fires after every finished load.
    pub fn on_load(&self, callback: impl Fn(Option<usize>) + 'static) {
        *self.on_load.borrow_mut() = Some(Rc::new(callback));
    }

    /// Speak-from-selection: set the callback the
    /// page's selection reports go through — `(speak_now, selected_text)`.
    pub fn on_selection(&self, callback: impl Fn(bool, String) + 'static) {
        *self.on_selection.borrow_mut() = Some(Rc::new(callback));
    }

    /// Apply an appearance: the engine's font settings, and the CSS layer
    /// over the page.
    pub fn set_appearance(&self, appearance: EpubAppearance) {
        *self.appearance.borrow_mut() = appearance.clone();
        if appearance.respect_styles {
            // The book's own design is the law here, and the revert is the
            // point: the engine's fallback fonts go back to the factory
            // defaults, so a size or a family the reader pushed while the
            // toggle was off does not leak into a respected book. The JS
            // layer skips its overrides for the same flag (see the script
            // below); this is the engine-settings half of the same rule.
            self.settings
                .set_default_font_family(&self.engine_default_family);
            self.settings.set_default_font_size(self.engine_default_size);
            self.settings
                .set_minimum_font_size(self.engine_default_min_size);
        } else {
            // The engine's own font settings are the *fallback*: a book that
            // names a family keeps it, and the reader's choice applies to
            // everything else. This is calibre's `apply_font_settings`.
            let family = if !appearance.font_family.trim().is_empty() {
                appearance.font_family.trim().to_string()
            } else {
                match appearance.font.as_str() {
                    "serif" => "serif".to_string(),
                    "sans" => "sans-serif".to_string(),
                    "mono" => "monospace".to_string(),
                    _ => "serif".to_string(),
                }
            };
            self.settings.set_default_font_family(&family);
            self.settings
                .set_default_font_size(appearance.font_size.round().max(1.0) as u32);
            self.settings
                .set_minimum_font_size(appearance.min_font_size.round().max(1.0) as u32);
        }
        // The page's own background: the window's pane around the book is
        // the scheme's colour, so a dark book does not sit on a white
        // rectangle — unless the book's own styles rule, in which case the
        // page is the publisher's paper (white), whatever the app's theme
        // is: the same stand-down the script below performs, and for the
        // same reason. A book that paints its own backgrounds (a themed
        // contents page) must read with ITS colour pair, not the scheme's
        // ink inherited onto its ground.
        let scheme = scheme_for(&appearance.theme);
        let ground = if appearance.respect_styles {
            "#ffffff".to_string()
        } else {
            scheme.background.to_string()
        };
        let rgba = gtk4::gdk::RGBA::parse(&ground).unwrap_or(gtk4::gdk::RGBA::WHITE);
        self.view.set_background_color(&rgba);
        self.apply_to_page();
    }

    /// The appearance as it stands.
    pub fn appearance(&self) -> EpubAppearance {
        self.appearance.borrow().clone()
    }

    /// Push the appearance into the loaded page.
    fn apply_to_page(&self) {
        let appearance = self.appearance.borrow().clone();
        let options = appearance_options(&appearance);
        let script = format!("window.__sn && window.__sn.apply({options});");
        let view = self.view.clone();
        self.view
            .evaluate_javascript(&script, None, None, None::<&gtk4::gio::Cancellable>, move |result| {
                if let Err(e) = result {
                    tracing::debug!(error = %e, "the book page took no appearance");
                }
            });
        let _ = view;
    }

    /// Turn a page in a paged book. `delta` is -1 or 1.
    pub fn turn_page(&self, delta: i32) {
        let script = format!("window.__sn && window.__sn.turn({delta});");
        self.view
            .evaluate_javascript(&script, None, None, None::<&gtk4::gio::Cancellable>, |result| {
                if let Err(e) = result {
                    tracing::debug!(error = %e, "the book page did not turn");
                }
            });
    }

    /// Scroll to an anchor inside the current page.
    pub fn scroll_to_fragment(&self, fragment: &str) {
        if fragment.is_empty() {
            return;
        }
        let script = format!(
            "window.__sn && window.__sn.scrollTo({});",
            json_string(fragment)
        );
        self.view
            .evaluate_javascript(&script, None, None, None::<&gtk4::gio::Cancellable>, |result| {
                if let Err(e) = result {
                    tracing::debug!(error = %e, "the book page did not scroll");
                }
            });
    }

    /// Fixed 2026-11-01: the page's scroll position as the
    /// page last reported it — `(y, max)`. `None` before the page's first
    /// report (it reports once its layout has landed, and on every scroll).
    pub fn scroll_state(&self) -> Option<(f64, f64)> {
        self.scroll_state.get()
    }

    /// Fixed 2026-11-01: scroll the page by a wheel notch —
    /// a twelfth of the viewport — `direction` above 0 goes down, below 0
    /// up. The page clamps itself; the scroll event it fires reports the
    /// new position back for the next keypress.
    pub fn scroll_by(&self, direction: f64) {
        let script = format!("window.__sn && window.__sn.scrollBy({direction});");
        self.view
            .evaluate_javascript(&script, None, None, None::<&gtk4::gio::Cancellable>, |result| {
                if let Err(e) = result {
                    tracing::debug!(error = %e, "the book page did not scroll");
                }
            });
    }
}

impl Clone for EpubView {
    /// A clone is a handle, not a copy: the web engine, the archive, the
    /// appearance and the callback slots are shared with every clone.
    // Handle-sharing fix: this used to
    // copy `appearance`/`current`/`on_load`/`on_selection` into fresh
    // RefCells, so a clone captured before a setter ran kept the old value
    // forever — the script-message handler never saw the reader's
    // `on_selection`. The Rc-shared fields make the doc true.
    fn clone(&self) -> Self {
        Self {
            view: self.view.clone(),
            settings: self.settings.clone(),
            state: self.state.clone(),
            appearance: self.appearance.clone(),
            current: self.current.clone(),
            on_load: self.on_load.clone(),
            on_selection: self.on_selection.clone(),
            scroll_state: self.scroll_state.clone(),
            engine_default_family: self.engine_default_family.clone(),
            engine_default_size: self.engine_default_size,
            engine_default_min_size: self.engine_default_min_size,
        }
    }
}

// ---------------------------------------------------------------------------
// Serving
// ---------------------------------------------------------------------------

/// Answer one request from the open book.
///
/// Only `/book/<path>` is served. The path is percent-decoded and looked
/// up by exact name — an EPUB's zip entries are case-sensitive, a parsed
/// document answers its `chapter-N`/`img/<name>` names — and a request
/// that names something the book does not contain gets a 404 rather than
/// a guess.
fn serve_request(state: &BookState, req: &webkit6::URISchemeRequest) {
    let Some(path) = req.path() else {
        finish_missing(req, "no path");
        return;
    };
    let path = path.to_string();
    let Some(rest) = path.strip_prefix('/') else {
        finish_missing(req, "no path");
        return;
    };
    // The URI is `speechnotes://book/<path>`, so the path arrives as
    // `/<path>`; the host is checked by the caller.
    let name = sn_doc::epub::percent_decode(rest);
    let source = state.source.borrow();
    let Some(source) = source.as_ref() else {
        finish_missing(req, "no book is open");
        return;
    };
    let Some(bytes) = source.read(&name) else {
        tracing::debug!(entry = %name, "the book has no such entry");
        finish_missing(req, "not in this book");
        return;
    };
    // An EPUB's entries carry the type their extension names; a parsed
    // document's chapter pages are XHTML and its images the parse's own.
    let content_type = match source {
        BookSource::Zip(_) => sn_doc::epub::mime_for(&name).to_string(),
        BookSource::Parts(parts) => parts.mime_for(&name),
    };
    let stream = gtk4::gio::MemoryInputStream::from_bytes(&gtk4::glib::Bytes::from_owned(bytes));
    req.finish(&stream, -1, Some(content_type.as_str()));
}

/// Refuse a request with a 404.
fn finish_missing(req: &webkit6::URISchemeRequest, why: &str) {
    let mut error = gtk4::glib::Error::new(
        gtk4::gio::IOErrorEnum::NotFound,
        &format!("not found: {why}"),
    );
    req.finish_error(&mut error);
}

/// The link schemes a book is allowed to hand to the desktop.
///
/// `http`, `https` and `mailto` only — the same rule the note preview
/// follows. Everything else is refused, so opening a book cannot become a
/// way to run something on the machine that opened it.
pub fn is_safe_link(href: &str) -> bool {
    let lower = href.trim().to_ascii_lowercase();
    ["http://", "https://", "mailto:"]
        .iter()
        .any(|scheme| lower.starts_with(scheme))
}

/// Percent-encode a zip path for a URL: everything outside the unreserved
/// set is escaped, so a name with a space or a `#` in it survives.
pub fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// The appearance as the JavaScript layer wants it: a JSON object of the
/// knobs, so the script never has to parse a string.
fn appearance_options(appearance: &EpubAppearance) -> String {
    let scheme = scheme_for(&appearance.theme);
    format!(
        "{{\"theme\":{},\"bg\":{},\"fg\":{},\"link\":{},\"dark\":{},\
         \"fontSize\":{},\"minFontSize\":{},\"letterSpacing\":{},\"lineHeight\":{},\
         \"paraGap\":{},\"marginTop\":{},\"marginBottom\":{},\"marginLeft\":{},\
         \"marginRight\":{},\"maxWidth\":{},\"flow\":{},\"columns\":{},\
         \"overrideColors\":{},\"respectStyles\":{}}}",
        json_string(&appearance.theme),
        json_string(scheme.background),
        json_string(scheme.foreground),
        json_string(scheme.link),
        scheme.dark,
        appearance.font_size,
        appearance.min_font_size,
        appearance.letter_spacing,
        appearance.line_height,
        appearance.paragraph_spacing,
        appearance.margin_top,
        appearance.margin_bottom,
        appearance.margin_left,
        appearance.margin_right,
        appearance.max_width,
        json_string(&appearance.flow),
        appearance.columns,
        json_string(&appearance.override_colors),
        appearance.respect_styles,
    )
}

/// A JSON string literal, quotes and all.
pub fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ---------------------------------------------------------------------------
// The injected script
// ---------------------------------------------------------------------------

/// The reader's script, injected at document start into every page of the
/// book.
///
/// It defines `window.__sn.apply(options)`, which lays the reader's
/// appearance over the document the way calibre's `apply_settings` does:
///
/// * the scheme's colours as CSS variables and as the document's own
///   foreground — a book that sets its own colours keeps them, because the
///   override rules are only added when the reader is told to;
/// * the base font size on the document element, so the book's `em` sizes
///   scale with it;
/// * the margins around the text, as padding on the document element, so
///   the book's own body margin survives;
/// * the reader's typographic overrides **only** when the book's own
///   styles are not respected.
const VIEWER_SCRIPT: &str = r##"
(function () {
    if (window.__sn) { return; }
    var cfg = {
        theme: "light", bg: "#ffffff", fg: "#000000", link: "#1a5fb4", dark: false,
        fontSize: 18, minFontSize: 10, letterSpacing: 0, lineHeight: 1.55, paraGap: 10,
        marginTop: 16, marginBottom: 16, marginLeft: 24, marginRight: 24,
        maxWidth: 0, flow: "flow", columns: 1,
        overrideColors: "never", respectStyles: true
    };

    function styleElement() {
        var ss = document.getElementById("sn-viewer-style");
        if (!ss) {
            ss = document.createElement("style");
            ss.id = "sn-viewer-style";
            (document.head || document.documentElement).appendChild(ss);
        }
        return ss;
    }

    function apply(c) {
        for (var k in c) { if (Object.prototype.hasOwnProperty.call(c, k)) { cfg[k] = c[k]; } }

        var de = document.documentElement.style;
        // Calibre's mechanism: the base size on the document element, so
        // every `em` in the book's own CSS scales with it — but only while
        // the book's own styles are NOT respected. Respecting means the
        // revert too: an inline size an earlier apply() pushed is cleared,
        // so the book's own sizing stands again.
        if (cfg.respectStyles) {
            de.removeProperty("font-size");
            de.removeProperty("letter-spacing");
        } else if (cfg.fontSize) {
            de.fontSize = cfg.fontSize + "px";
        }
        if (cfg.respectStyles) {
            // The book's own design stands alone: the reader paints
            // NOTHING. Forcing the scheme's ink onto the document made any
            // element that paints its own background — a themed contents
            // page, a decorated cover — read scheme-ink-on-its-own-dark-
            // ground, which in a light theme is dark-on-dark: the page
            // looked blank. The scheme's colours are the reader's FALLBACK
            // (the branch below, for when the reader supplies the design),
            // never a layer over a book the reader is told to respect.
            // The page renders as the publisher shipped it, on its own
            // paper, in every app theme — that IS the reset.
            de.removeProperty("--sn-bg");
            de.removeProperty("--sn-fg");
            de.removeProperty("--sn-link");
            de.removeProperty("color");
            de.removeProperty("color-scheme");
        } else {
            de.setProperty("--sn-bg", cfg.bg);
            de.setProperty("--sn-fg", cfg.fg);
            de.setProperty("--sn-link", cfg.link);
            de.colorScheme = cfg.dark ? "dark" : "light";
            de.color = cfg.fg;
        }

        var body = document.body;
        if (body) {
            if (cfg.respectStyles) {
                // An earlier !respect apply may have left inline colours
                // behind; the book's own cascade takes over.
                body.style.removeProperty("color");
                body.style.removeProperty("background-color");
            } else {
                body.style.color = cfg.fg;
                body.style.backgroundColor = "transparent";
            }
            body.classList.remove("sn-light-colors", "sn-dark-colors");
            body.classList.add(cfg.dark ? "sn-dark-colors" : "sn-light-colors");
        }
        document.documentElement.classList.add("sn-viewer");

        var css = [];
        // The margins are the space *around* the book: padding on the
        // document element, so the publisher's own body margin is untouched.
        css.push("html{padding:" +
            cfg.marginTop + "px " + cfg.marginRight + "px " + cfg.marginBottom + "px " +
            cfg.marginLeft + "px !important;box-sizing:border-box !important;}");
        // The canvas is the scheme's only while the reader supplies the
        // design; respected, the page shows its own (the widget behind it
        // stands down to paper-white with it — see set_appearance).
        if (!cfg.respectStyles) {
            css.push("html{background-color:var(--sn-bg) !important;}");
        }

        if (cfg.maxWidth > 0) {
            css.push("body{max-width:" + cfg.maxWidth + "px !important;margin-left:auto !important;margin-right:auto !important;}");
        }

        if (!cfg.respectStyles) {
            css.push("body{line-height:" + cfg.lineHeight + " !important;}");
            css.push("body p,body div,body li,body td,body th{margin-bottom:" + cfg.paraGap + "px !important;}");
        }
        if (cfg.letterSpacing > 0 && !cfg.respectStyles) {
            css.push("body{letter-spacing:" + cfg.letterSpacing + "px !important;}");
        }

        // The preserve-or-retheme switch, in calibre's three states.
        if (cfg.overrideColors === "always" || (cfg.overrideColors === "dark" && cfg.dark)) {
            css.push("body.sn-dark-colors *,body.sn-light-colors *{color:var(--sn-fg) !important;background-color:var(--sn-bg) !important;border-color:var(--sn-fg) !important;}");
            css.push("html > body :link,html > body :link *{color:var(--sn-link) !important;}");
        }

        if (cfg.flow === "paged") {
            var vw = window.innerWidth, vh = window.innerHeight;
            css.push("html{height:100% !important;overflow-x:auto !important;overflow-y:hidden !important;padding:0 !important;}");
            css.push("body{height:" + vh + "px !important;column-width:" + vw + "px !important;column-gap:0 !important;column-fill:auto !important;overflow:visible !important;}");
        } else {
            css.push("html{height:auto !important;overflow-x:auto !important;overflow-y:auto !important;}");
            css.push("body{height:auto !important;column-width:auto !important;column-gap:normal !important;column-fill:auto !important;}");
        }

        styleElement().textContent = css.join("\n");

        // Fixed 2026-11-01: the keyboard's first Down needs
        // to know this page scrolls at all — report where it stands once
        // the layer has laid out.
        setTimeout(snReportScroll, 60);
    }

    // Speak-from-selection: selection → "Speak from
    // here". A selection that stays still for a beat shows a small pill at
    // its head; clicking it posts the selection's text. Every settled
    // selection is posted too, so the app's F5 can start from it without
    // the pill.
    var snBubble = null;
    var snSelText = "";
    function snPost(msg) {
        try { window.webkit.messageHandlers.snebook.postMessage(msg); } catch (e) {}
    }
    function snHideBubble() { if (snBubble) { snBubble.remove(); snBubble = null; } }
    function snShowBubble(cx, cy) {
        snHideBubble();
        snBubble = document.createElement("div");
        snBubble.textContent = "Speak from here";
        snBubble.style.cssText = "position:fixed;z-index:2147483647;" +
            "background:rgba(28,28,30,0.85);color:#fff;font:13px sans-serif;" +
            "padding:4px 10px;border-radius:12px;cursor:pointer;" +
            "user-select:none;-webkit-user-select:none;" +
            "box-shadow:0 1px 4px rgba(0,0,0,0.35);";
        snBubble.style.left = Math.max(4, cx - 60) + "px";
        snBubble.style.top = Math.max(4, cy - 34) + "px";
        snBubble.addEventListener("mousedown", function (ev) {
            ev.preventDefault(); ev.stopPropagation();
            snPost("speak:" + snSelText);
            snHideBubble();
        });
        (document.body || document.documentElement).appendChild(snBubble);
    }
    var snSelTimer = null;
    document.addEventListener("selectionchange", function () {
        if (snSelTimer) { clearTimeout(snSelTimer); }
        snSelTimer = setTimeout(function () {
            snSelTimer = null;
            var sel = window.getSelection && window.getSelection();
            snSelText = (sel && !sel.isCollapsed) ? String(sel) : "";
            snPost("sel:" + snSelText);
            if (!snSelText) { snHideBubble(); return; }
            try {
                var r = sel.getRangeAt(0).getBoundingClientRect();
                snShowBubble(r.left + r.width / 2, r.top);
            } catch (e) { snHideBubble(); }
        }, 300);
    });

    // Fixed 2026-11-01: the page's scroll position, reported
    // as the page scrolls — `scroll:<y>:<max>`. The app's Up/Down decide
    // between scrolling the page and turning the chapter from this.
    // Throttled: one smooth scroll fires dozens of events.
    function snReportScroll() {
        try {
            var de = document.documentElement;
            var max = Math.max(0, (de.scrollHeight || 0) - (window.innerHeight || 0));
            snPost("scroll:" + (window.scrollY || 0) + ":" + max);
        } catch (e) {}
    }
    var snScrollTimer = null;
    window.addEventListener("scroll", function () {
        if (cfg.flow === "paged") { return; }
        if (snScrollTimer) { return; }
        snScrollTimer = setTimeout(function () {
            snScrollTimer = null;
            snReportScroll();
        }, 120);
    }, true);

    // The resize drift (2026-11-13): a paged book's page IS a column
    // of the old window's width. A resize changes the column width while the
    // browser keeps scrollX, so the view lands between columns — pages away
    // from where the reader stood (60 → 40 or 90, sidebar collapse,
    // fullscreen, whatever moved the window). Once the burst of resize
    // events settles, the reader's page — `scrollX / oldVw` — is re-landed
    // at the new width's own boundary. The pre-burst width is kept until the
    // burst ends: recomputing per event would drift with each intermediate
    // width. Flow mode needs nothing — the browser keeps the vertical
    // position and reflows around it.
    var snLastVw = window.innerWidth || 0;
    var snResizeTimer = null;
    window.addEventListener("resize", function () {
        if (cfg.flow !== "paged") { snLastVw = window.innerWidth || 0; return; }
        if (snResizeTimer) { clearTimeout(snResizeTimer); }
        snResizeTimer = setTimeout(function () {
            snResizeTimer = null;
            var oldVw = snLastVw || window.innerWidth || 1;
            var vw = window.innerWidth || 1;
            snLastVw = vw;
            var page = Math.floor((window.scrollX || 0) / oldVw);
            var max = Math.max(0, document.documentElement.scrollWidth - vw);
            var x = Math.min(max, Math.max(0, page * vw));
            if (Math.abs((window.scrollX || 0) - x) > 1) { window.scrollTo(x, 0); }
        }, 80);
    });

    window.__sn = {
        cfg: cfg,
        apply: apply,
        scrollBy: function (direction) {
            if (cfg.flow === "paged") { return; }
            window.scrollBy(0, direction * 0.12 * (window.innerHeight || 1));
        },
        turn: function (delta) {
            if (cfg.flow !== "paged") { return; }
            var vw = window.innerWidth;
            var max = Math.max(0, document.documentElement.scrollWidth - vw);
            var x = Math.min(max, Math.max(0, window.scrollX + delta * vw));
            window.scrollTo(x, 0);
        },
        page: function () {
            var vw = window.innerWidth || 1;
            var pages = Math.max(1, Math.ceil(document.documentElement.scrollWidth / vw));
            var page = Math.min(pages - 1, Math.floor(window.scrollX / vw));
            return { page: page, pages: pages };
        },
        scrollTo: function (fragment) {
            var el = null;
            try { el = document.getElementById(fragment) || document.querySelector("[name='" + fragment + "']"); }
            catch (e) { el = null; }
            if (el && el.scrollIntoView) { el.scrollIntoView(); }
        }
    };
})();
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_encoded_for_a_url() {
        assert_eq!(encode_path("OEBPS/ch01.xhtml"), "OEBPS/ch01.xhtml");
        assert_eq!(encode_path("OEBPS/my chapter.xhtml"), "OEBPS/my%20chapter.xhtml");
        assert_eq!(encode_path("OEBPS/a#b.xhtml"), "OEBPS/a%23b.xhtml");
    }

    #[test]
    fn the_schemes_are_the_ones_the_family_shares() {
        assert_eq!(scheme_for("sepia").background, "#F6F3E9");
        assert!(scheme_for("dark").dark);
        assert!(scheme_for("trueBlack").dark);
        assert!(!scheme_for("light").dark);
        // An unknown theme is the light one, not a broken scheme.
        assert_eq!(scheme_for("nonsense").background, "#FFFFFF");
    }

    #[test]
    fn only_the_desktop_schemes_are_followed() {
        assert!(is_safe_link("https://example.org/x"));
        assert!(is_safe_link("mailto:a@b.c"));
        assert!(!is_safe_link("file:///etc/passwd"));
        assert!(!is_safe_link("javascript:alert(1)"));
    }

    #[test]
    fn a_clone_shares_the_callback_slots_with_the_original() {
        //  the manual Clone used to copy
        // the callback RefCells, so a handler that captured its own clone at
        // connect time never saw the callback the reader attached later —
        // speak-from-selection messages arrived and were dropped on the
        // floor. This pins the handle semantics the doc always claimed.
        // (Guarded: two webview tests in one process — a second gtk4::init()
        // is an error, and test order must not matter.)
        if !gtk4::is_initialized() {
            gtk4::init().expect("gtk init for the web view");
        }
        let view = EpubView::new();
        let handle = view.clone();
        view.on_selection(|_, _| {});
        assert!(
            handle.on_selection.borrow().is_some(),
            "a clone must share the on_selection slot"
        );
        view.on_load(|_| {});
        assert!(
            handle.on_load.borrow().is_some(),
            "a clone must share the on_load slot"
        );

        // "Respect the book's styles" is a revert, not just a gate: the
        // engine font settings the reader pushed while it was off come back
        // to the factory defaults the moment it goes on — a 28 px base left
        // over from the last session must not leak into a book the reader
        // now respects. (This lives in the module's ONE webview-constructing
        // test: two WebKit spawns in one test binary race the sandbox's bus
        // proxy on this box, measured 2026-11-12.)
        let factory_size = view.engine_default_size;
        let factory_family = view.engine_default_family.clone();
        let factory_min = view.engine_default_min_size;
        let mut override_book = EpubAppearance::default();
        override_book.respect_styles = false;
        override_book.font_size = 28.0;
        override_book.font = "sans".to_string();
        view.set_appearance(override_book);
        assert_eq!(view.settings.default_font_size(), 28);
        assert_eq!(
            view.settings.default_font_family().as_deref(),
            Some("sans-serif")
        );
        let mut respect_book = EpubAppearance::default();
        respect_book.respect_styles = true;
        respect_book.font_size = 28.0;
        respect_book.font = "sans".to_string();
        view.set_appearance(respect_book);
        assert_eq!(
            view.settings.default_font_size(),
            factory_size,
            "the factory size is back"
        );
        assert_eq!(
            view.settings.default_font_family().as_deref(),
            Some(factory_family.as_str()),
            "the factory family is back"
        );
        assert_eq!(view.settings.minimum_font_size(), factory_min);
    }

    #[test]
    fn the_options_are_json() {
        let appearance = EpubAppearance::default();
        let options = appearance_options(&appearance);
        assert!(options.starts_with('{'));
        assert!(options.contains("\"theme\":\"light\""));
        assert!(options.contains("\"respectStyles\":true"));
    }
}
