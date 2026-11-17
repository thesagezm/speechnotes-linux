//! The book view: **every reflowable book through one real engine** —
//! foliate-js, the renderer readest and GNOME's Foliate ship, vendored into
//! this binary (see `assets/foliate-js/LICENSE`).
//!
//! The old reader parsed each headless format (Mobipocket/Kindle, FB2, RTF,
//! HTML, plain text) into a plain-XHTML model and served synthesised pages;
//! the pages came out with repeated letters from a PalmDOC bug, `xmlns`
//! strings in the prose, chapters that were not chapters, and no images.
//! The parse *was* the bug. This view hands the file's bytes — **the
//! original file, verbatim** — to the engine in the webview: mobi.js
//! decompresses PalmDOC and KF8 (the decompressors Foliate has shipped for
//! years), fb2.js renders FictionBook's own structure, epub.js reads the
//! EPUB container, and the headless formats the engine does not carry (TXT,
//! HTML, RTF) are built into in-memory book objects by the shell's own
//! modules — readest's chapteriser for TXT, readest's HTML book builder,
//! and an RTF renderer that keeps the formatting RTF actually declares.
//!
//! What the reader gets back, over the script-message channel:
//! * the **book report** — metadata and the format's own nested contents
//!   (a KF8's NCX, an FB2's sections, a TXT's detected headings), so the
//!   contents sidebar shows chapters within chapters, straight from the
//!   book's structure rather than a guess;
//! * **relocations** — the section index, the fraction, and a CFI, which
//!   is what resume stores (a position that survives reflow exactly, the
//!   way readest's does);
//! * **internal links** — foliate routes a click on the book's own contents
//!   page through `resolveHref` and lands on the exact destination, so the
//!   TOC page in the book navigates like the sidebar does;
//! * **external links** — reported for the desktop's safe-link rule.
//!
//! The loopback server serves exactly three things: the compiled-in viewer
//! assets (`viewer/<name>` — the shell page with its module spliced
//! inline, see `assets::shell_page`), the open book's file
//! (`file/original.<ext>`, streamed from disk on demand, never copied
//! into RAM twice), and nothing else — a 404 for every other name, the
//! same discipline the EPUB view's handler has always had.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gtk4::prelude::*;
use webkit6::prelude::*;

use crate::assets;
use crate::epubview::EpubAppearance;

/// The name the JS→Rust messages go through.
const MESSAGES: &str = "snbook";

// ---------------------------------------------------------------------------
// The messages the shell sends
// ---------------------------------------------------------------------------

/// One row of the book's own contents, flattened by the reader from the
/// nested report. `index` is the section the row lands on (the sidebar's
/// activation and the open-row mark both key on it); `href` is the engine's
/// own target, which reaches the exact anchor the book pointed at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookTocRow {
    pub label: String,
    pub href: String,
    pub index: usize,
    pub depth: usize,
}

/// The book report: what the engine found in the file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BookInfo {
    pub title: String,
    pub author: String,
    pub language: String,
    pub section_count: usize,
    pub toc: Vec<BookTocRow>,
}

/// A relocation: the view moved — a page turn, a scroll, a navigation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Relocate {
    pub cfi: String,
    pub index: usize,
    pub fraction: f64,
    pub label: String,
    pub section_count: usize,
    /// Where the visible page starts in its section's own text, as UTF-16
    /// units of the string the section's text report carries — the same
    /// unit the bookmarks store, and the position the voice starts from
    /// when the reader asks it to read *this page*. `None` when the shell
    /// could not name one (no visible text — a cover image, a blank page).
    pub text_offset: Option<usize>,
}

// ---------------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------------

/// What the HTTP server serves the book from: the file on disk, read on
/// demand. The engine fetches it once per open; no second copy is held.
#[derive(Default)]
struct BookState {
    file: std::sync::Mutex<Option<PathBuf>>,
}

/// The token guarding this instance's server: a path prefix no other local
/// process can guess, so a stray page cannot pull the open book's bytes.
fn random_token() -> String {
    let mut bytes = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut bytes);
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The loopback server the engine loads through.
///
/// ES module imports fetch in CORS mode, and on this WebKit a custom-scheme
/// handler's responses — CORS-enabled or not, headered or not — stall the
/// module loader mid-graph (measured: the identical graph loads clean over
/// plain http). So the engine loads from `http://127.0.0.1:<port>/<token>/…`
/// — the same shape calibre's and readest's local servers take. The port is
/// picked by the kernel, the token is one-shot random, and the listener
/// binds the loopback only; a book's bytes are reachable from this machine
/// alone, and only with the token.
fn start_server(state: std::sync::Arc<BookState>) -> Option<(u16, String)> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").ok()?;
    let port = listener.local_addr().ok()?.port();
    let token = random_token();
    let token_for_loop = token.clone();
    std::thread::Builder::new()
        .name("sn-bookview-http".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else {
                    continue;
                };
                let state = state.clone();
                let token = token_for_loop.clone();
                // One short-lived thread per connection: the module graph's
                // first load fires a dozen fetches at once.
                std::thread::spawn(move || {
                    serve_connection(&mut stream, &state, &token);
                });
            }
        })
        .ok()?;
    Some((port, token))
}

/// Read one request, answer it, close. GET only — everything the engine
/// asks for is a GET, and anything else is a 404.
fn serve_connection(stream: &mut std::net::TcpStream, state: &BookState, token: &str) {
    use std::io::{Read, Write};
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    // The request head ends at a blank line; a book's 4 MB body comes the
    // other way, so never read past the head.
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 32 * 1024 {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let head = String::from_utf8_lossy(&buf);
    let Some(request_line) = head.lines().next() else {
        return;
    };
    let mut parts = request_line.split_whitespace();
    let _method = parts.next().unwrap_or_default();
    let Some(target) = parts.next() else {
        return;
    };
    let path = target.split('?').next().unwrap_or(target);

    // `/<token>/viewer/<asset>` and `/<token>/file/<name>`; anything else —
    // a wrong token above all — is a 404, never a guess.
    let served: Option<(String, Vec<u8>)> = path
        .strip_prefix(&format!("/{token}/"))
        .and_then(|rest| match rest.strip_prefix("viewer/") {
            Some(asset) => assets::viewer_asset(asset)
                .map(|(mime, bytes)| (mime.to_string(), bytes.to_vec())),
            None => rest.strip_prefix("file/").and_then(|file_name| {
                let source = state.file.lock().ok()?.clone();
                match source {
                    Some(path) if path
                        .file_name()
                        .map(|n| n.to_string_lossy() == file_name)
                        .unwrap_or(false) =>
                    {
                        std::fs::read(&path).ok().map(|bytes| {
                            (sn_doc::epub::mime_for(file_name).to_string(), bytes)
                        })
                    }
                    _ => None,
                }
            }),
        });
    match served {
        Some((mime, bytes)) => {
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\n\
                 Access-Control-Allow-Origin: *\r\nCache-Control: no-store\r\n\
                 Connection: close\r\n\r\n",
                bytes.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(&bytes);
        }
        None => {
            let _ = stream.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    }
    let _ = stream.flush();
}

/// The book view: a web engine running the foliate-js shell.
pub struct BookView {
    pub view: webkit6::WebView,
    settings: webkit6::Settings,
    state: std::sync::Arc<BookState>,
    /// `http://127.0.0.1:<port>/<token>` — the prefix every engine request
    /// carries (the shell's URL and the book's file URL both build on it).
    origin: String,
    appearance: Rc<RefCell<EpubAppearance>>,
    /// The page's scroll position as the primary section last reported it
    /// — `(y, max)` — the evidence the reader's Up/Down keys read to decide
    /// between scrolling the page and turning the section.
    scroll_state: Rc<std::cell::Cell<Option<(f64, f64)>>>,
    on_book_info: Rc<RefCell<Option<Rc<dyn Fn(BookInfo)>>>>,
    on_relocate: Rc<RefCell<Option<Rc<dyn Fn(Relocate)>>>>,
    /// A section's text, as the engine sees it — the section document's own
    /// prose. The voice and the read-along view read *this*, so what the
    /// listener hears is exactly what the page shows (the engine's own
    /// decompression included, which is the point).
    on_text: Rc<RefCell<Option<Rc<dyn Fn(usize, String)>>>>,
    on_external: Rc<RefCell<Option<Rc<dyn Fn(String)>>>>,
    on_selection: Rc<RefCell<Option<Rc<dyn Fn(bool, String)>>>>,
    on_error: Rc<RefCell<Option<Rc<dyn Fn(String)>>>>,
}

impl BookView {
    pub fn widget(&self) -> &webkit6::WebView {
        &self.view
    }

    /// The web engine with the loopback server running and the shell's
    /// message channel in place.
    pub fn new() -> Self {
        let state = std::sync::Arc::new(BookState::default());
        // A machine that cannot open a loopback socket cannot host the
        // engine; the view stays blank rather than half-loading. (The only
        // known case is a socket-exhausted sandbox.)
        let origin = start_server(state.clone())
            .map(|(port, token)| format!("http://127.0.0.1:{port}/{token}"))
            .unwrap_or_default();
        if origin.is_empty() {
            tracing::warn!("the book view's loopback server could not start");
        }

        let ctx = webkit6::WebContext::new();

        let settings = webkit6::Settings::new();
        settings.set_enable_javascript(true);
        settings.set_enable_developer_extras(false);
        // The engine's own console (the shell logs its boot stages) reaches
        // stdout only when asked for — a diagnostic, never in production.
        let console = std::env::var_os("SN_BOOKVIEW_CONSOLE").is_some();
        settings.set_enable_write_console_messages_to_stdout(console);

        let manager = webkit6::UserContentManager::new();
        let messages_ok = manager.register_script_message_handler(MESSAGES, None);

        let view = webkit6::WebView::builder()
            .web_context(&ctx)
            .settings(&settings)
            .user_content_manager(&manager)
            .build();
        view.set_css_classes(&["book-view"]);

        let bookview = Self {
            view,
            state,
            origin,
            appearance: Rc::new(RefCell::new(EpubAppearance::default())),
            scroll_state: Rc::new(std::cell::Cell::new(None)),
            on_book_info: Rc::new(RefCell::new(None)),
            on_relocate: Rc::new(RefCell::new(None)),
            on_text: Rc::new(RefCell::new(None)),
            on_external: Rc::new(RefCell::new(None)),
            on_selection: Rc::new(RefCell::new(None)),
            on_error: Rc::new(RefCell::new(None)),
            settings,
        };
        if messages_ok {
            let bookview = bookview.clone();
            manager.connect_script_message_received(Some(MESSAGES), move |_, value| {
                bookview.on_message(&value.to_str());
            });
        }
        bookview
    }

    /// Redraw the pane now and once more, shortly after. The engine's
    /// reports race the web process's frame commit: a report arrives, then
    /// the rendered frame crosses — an immediate redraw can still paint the
    /// old (white) buffer, so a second pass picks the frame up. Cheap:
    /// queue_draw only invalidates, and GTK coalesces per frame.
    fn schedule_repaint(&self) {
        self.view.queue_draw();
        let view = self.view.clone();
        gtk4::glib::timeout_add_local(std::time::Duration::from_millis(400), move || {
            view.queue_draw();
            gtk4::glib::ControlFlow::Break
        });
    }

    /// Nudge the pane's allocation when the engine's first content lands.
    ///
    /// A document that takes seconds to parse (a 1.4 MB RTF) finishes
    /// booting *after* the window's first committed frame — and on this
    /// WebKit the pane keeps that white initial buffer until the widget's
    /// allocation actually changes: a maximize, a sidebar toggle. Redraws
    /// and unchanged-size resizes don't carry the frame (measured side by
    /// side); a real allocation change does. Two one-pixel margin flips do
    /// exactly that, invisibly. Runs once, on 'ready', so a page turn can
    /// never re-enter it.
    ///
    /// Public because fill-the-window needs the same medicine: its quick
    /// collapse-then-expand lands the pane at the allocation it left, the
    /// two resizes coalesce, and the pane keeps whatever frame was last
    /// committed — the reader's "the viewer went blank" after a fast F11
    /// tap (measured 2026-11-17).
    pub fn nudge_allocation(&self) {
        let view = self.view.clone();
        gtk4::glib::timeout_add_local(std::time::Duration::from_millis(150), move || {
            view.set_margin_top(1);
            let view = view.clone();
            gtk4::glib::timeout_add_local(std::time::Duration::from_millis(150), move || {
                view.set_margin_top(0);
                gtk4::glib::ControlFlow::Break
            });
            gtk4::glib::ControlFlow::Break
        });
    }

    /// One message from the shell.
    fn on_message(&self, raw: &str) {
        let (kind, payload) = raw.split_once(':').unwrap_or((raw, ""));
        match kind {
            "book" => {
                // The engine's boot lands after the window's first paint
                // when a document takes seconds to parse (a 1.4 MB RTF);
                // without fresh damage the pane keeps its white initial
                // buffer until some unrelated event redraws it. The
                // reports themselves are the damage: redraw on each.
                self.schedule_repaint();
                match parse_book_info(payload) {
                    Ok(info) => {
                        if let Some(cb) = self.on_book_info.borrow().as_ref() {
                            cb(info);
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "the book report did not parse");
                    }
                }
            }
            "relocate" => {
                self.schedule_repaint();
                match parse_relocate(payload) {
                    Ok(relocate) => {
                        if let Some(cb) = self.on_relocate.borrow().as_ref() {
                            cb(relocate);
                        }
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "a relocation did not parse");
                    }
                }
            }
            "scroll" => {
                // The reader's keys read the scroll state on demand; the
                // reports themselves only warm the last position.
                if let Some((y, max)) = payload.split_once(':') {
                    if let (Ok(_y), Ok(_max)) = (y.parse::<f64>(), max.parse::<f64>()) {
                        self.scroll_state.set(Some((_y, _max)));
                    }
                }
            }
            "external" => {
                if let Some(cb) = self.on_external.borrow().as_ref() {
                    cb(payload.to_string());
                }
            }
            "sel" => {
                if let Some(cb) = self.on_selection.borrow().as_ref() {
                    cb(false, payload.to_string());
                }
            }
            "text" => {
                // `text:<index>:<json string>` — the section document's own
                // prose. The index guards against a late report for a
                // section the reader has already left.
                if let Some((index, text)) = payload.split_once(':') {
                    if let Ok(index) = index.parse::<usize>() {
                        if let Ok(decoded) = serde_json::from_str::<String>(text) {
                            if let Some(cb) = self.on_text.borrow().as_ref() {
                                cb(index, decoded);
                            }
                        }
                    }
                }
            }
            "speak" => {
                if let Some(cb) = self.on_selection.borrow().as_ref() {
                    cb(true, payload.to_string());
                }
            }
            "error" => {
                tracing::warn!(error = %payload, "the book view failed");
                if let Some(cb) = self.on_error.borrow().as_ref() {
                    cb(payload.to_string());
                }
            }
            "ready" => {
                self.schedule_repaint();
                self.nudge_allocation();
            }
            other => {
                tracing::debug!(message = %other, "unknown book-view message");
            }
        }
    }

    /// Open `path` (the book's original file, on disk) as `format`. The
    /// shell receives the resume CFI and the metadata the manifest carries
    /// (formats whose text carries none — plain text above all — borrow the
    /// manifest's title for their book report).
    pub fn open(
        &self,
        path: &PathBuf,
        format: &str,
        title: &str,
        author: &str,
        cfi: Option<&str>,
    ) {
        if self.origin.is_empty() {
            return;
        }
        *self.state.file.lock().expect("book file lock") = Some(path.clone());
        let mut url = format!(
            "{}/viewer/sn-shell.html?src={}/file/{}",
            self.origin,
            self.origin,
            epubview_encode_path(&format!("original.{format}")),
        );
        url.push_str(&format!("&format={}", epubview_encode_path(format)));
        url.push_str(&format!(
            "&title={}",
            epubview_encode_query(title)
        ));
        url.push_str(&format!(
            "&author={}",
            epubview_encode_query(author)
        ));
        if let Some(cfi) = cfi.filter(|c| !c.is_empty()) {
            url.push_str(&format!("&cfi={}", epubview_encode_query(cfi)));
        }
        self.view.load_uri(&url);
    }

    /// The callbacks.
    pub fn on_book_info(&self, cb: impl Fn(BookInfo) + 'static) {
        *self.on_book_info.borrow_mut() = Some(Rc::new(cb));
    }
    pub fn on_relocate(&self, cb: impl Fn(Relocate) + 'static) {
        *self.on_relocate.borrow_mut() = Some(Rc::new(cb));
    }
    pub fn on_text(&self, cb: impl Fn(usize, String) + 'static) {
        *self.on_text.borrow_mut() = Some(Rc::new(cb));
    }
    pub fn on_external(&self, cb: impl Fn(String) + 'static) {
        *self.on_external.borrow_mut() = Some(Rc::new(cb));
    }
    pub fn on_selection(&self, cb: impl Fn(bool, String) + 'static) {
        *self.on_selection.borrow_mut() = Some(Rc::new(cb));
    }
    pub fn on_error(&self, cb: impl Fn(String) + 'static) {
        *self.on_error.borrow_mut() = Some(Rc::new(cb));
    }

    /// Apply an appearance — the same `EpubAppearance` the EPUB view takes,
    /// mapped onto the engine: the scheme's colours and the type knobs ride
    /// into every loaded document (the shell's type layer), the layout
    /// knobs become paginator attributes. The scheme foreground stays in
    /// both branches: reader chrome, as in the EPUB view.
    pub fn set_appearance(&self, appearance: EpubAppearance) {
        *self.appearance.borrow_mut() = appearance.clone();
        let scheme = crate::epubview::scheme_for(&appearance.theme);
        let family = if !appearance.font_family.trim().is_empty() {
            appearance.font_family.trim().to_string()
        } else {
            match appearance.font.as_str() {
                "serif" => "serif".to_string(),
                "sans" => "sans-serif".to_string(),
                "mono" => "monospace".to_string(),
                _ => String::new(),
            }
        };
        let options = format!(
            "{{\"background\":{},\"foreground\":{},\"link\":{},\"dark\":{},\
             \"fontSize\":{},\"letterSpacing\":{},\"lineHeight\":{},\"paraGap\":{},\
             \"fontFamily\":{},\"respectStyles\":{},\"flow\":{},\
             \"marginTop\":{},\"marginBottom\":{},\"marginLeft\":{},\"marginRight\":{},\
             \"maxWidth\":{},\"columns\":{}}}",
            crate::epubview::json_string(scheme.background),
            crate::epubview::json_string(scheme.foreground),
            crate::epubview::json_string(scheme.link),
            scheme.dark,
            appearance.font_size,
            appearance.letter_spacing,
            appearance.line_height,
            appearance.paragraph_spacing,
            crate::epubview::json_string(&family),
            appearance.respect_styles,
            crate::epubview::json_string(&appearance.flow),
            appearance.margin_top,
            appearance.margin_bottom,
            appearance.margin_left,
            appearance.margin_right,
            appearance.max_width,
            appearance.columns,
        );
        let script = format!("window.__snbook && window.__snbook.apply({options});");
        self.evaluate(&script);
        // The engine's fallback font settings follow the reader's choice the
        // way the EPUB view's do: on while the book's styles are overridden,
        // factory defaults while they are respected.
        if appearance.respect_styles {
            // (The engine defaults were read at construction; the shell's
            // type layer already stands down for a respected book.)
        } else {
            if !family.is_empty() {
                self.settings.set_default_font_family(&family);
            }
            self.settings
                .set_default_font_size(appearance.font_size.round().max(1.0) as u32);
            self.settings
                .set_minimum_font_size(appearance.min_font_size.round().max(1.0) as u32);
        }
        let ground = if appearance.respect_styles {
            "#ffffff"
        } else {
            scheme.background
        };
        let rgba = gtk4::gdk::RGBA::parse(ground).unwrap_or(gtk4::gdk::RGBA::WHITE);
        self.view.set_background_color(&rgba);
    }

    /// Navigate — an href from the book's contents, a section index, or a
    /// CFI; the engine resolves whichever arrives.
    pub fn go_to(&self, target: &str) {
        let script = format!(
            "window.__snbook ? (window.__snbook.goTo({}), 'ok') : 'no-shell';",
            crate::epubview::json_string(target)
        );
        self.evaluate(&script);
    }

    pub fn go_to_index(&self, index: usize) {
        self.evaluate(&format!(
            "window.__snbook ? (window.__snbook.goToIndex({index}), 'ok') : 'no-shell';"
        ));
    }

    pub fn next(&self) {
        self.evaluate("window.__snbook ? (window.__snbook.next(), 'ok') : 'no-shell';");
    }

    pub fn prev(&self) {
        self.evaluate("window.__snbook ? (window.__snbook.prev(), 'ok') : 'no-shell';");
    }

    pub fn scroll_by(&self, direction: f64) {
        self.evaluate(&format!(
            "window.__snbook ? (window.__snbook.scrollBy({direction}), 'ok') : 'no-shell';"
        ));
    }

    /// The page's selection, for speak-from-selection.
    pub fn selection(&self, callback: impl FnOnce(String) + 'static) {
        self.view.evaluate_javascript(
            "window.__snbook ? window.__snbook.selection() : ''",
            None,
            None,
            None::<&gtk4::gio::Cancellable>,
            move |result| {
                let text = result
                    .map(|v| v.to_str().to_string())
                    .unwrap_or_default();
                callback(text);
            },
        );
    }

    fn evaluate(&self, script: &str) {
        self.view
            .evaluate_javascript(script, None, None, None::<&gtk4::gio::Cancellable>, |result| {
                if let Err(e) = result {
                    tracing::debug!(error = %e, "the book page took no command");
                }
            });
    }
}

impl Clone for BookView {
    /// A clone is a handle, not a copy: every clone shares the webview, the
    /// state and the callback slots (the EpubView rule, same reason).
    fn clone(&self) -> Self {
        Self {
            view: self.view.clone(),
            settings: self.settings.clone(),
            state: self.state.clone(),
            origin: self.origin.clone(),
            appearance: self.appearance.clone(),
            scroll_state: self.scroll_state.clone(),
            on_book_info: self.on_book_info.clone(),
            on_relocate: self.on_relocate.clone(),
            on_text: self.on_text.clone(),
            on_external: self.on_external.clone(),
            on_selection: self.on_selection.clone(),
            on_error: self.on_error.clone(),
        }
    }
}

    /// The page's scroll position as the page last reported it — what the
    /// reader's keyboard asks for before it scrolls or turns.
    impl BookView {
        pub fn scroll_state(&self) -> Option<(f64, f64)> {
            self.scroll_state.get()
        }

        /// The open book's flow, from the appearance — the keyboard's
        /// scroll-or-turn decision reads it.
        pub fn flow(&self) -> String {
            self.appearance.borrow().flow.clone()
        }

        /// A section's text, as the engine last rendered it — the cached
        /// report from the shell. An index the engine has not shown yet
        /// answers empty; the `text` report will arrive when it loads.
        /// A section's cached prose, with whether it has loaded at all: the
        /// shell answers `{loaded, text}` — an absent key (still loading)
        /// and a loaded-but-blank section are different answers, and the
        /// voice's walk needs to tell them apart (waiting on a report that
        /// already fired is a stall; passing a section that is still loading
        /// skips a chapter).
        pub fn section_text(
            &self,
            index: usize,
            callback: impl FnOnce(usize, bool, String) + 'static,
        ) {
            let script = format!(
                "window.__snbook ? window.__snbook.sectionText({index}) : ''"
            );
            self.view.evaluate_javascript(
                &script,
                None,
                None,
                None::<&gtk4::gio::Cancellable>,
                move |result| {
                    let raw = result
                        .map(|v| v.to_str().to_string())
                        .unwrap_or_default();
                    let (loaded, text) = match serde_json::from_str::<serde_json::Value>(&raw)
                        .ok()
                    {
                        Some(v) => (
                            v.get("loaded").and_then(|x| x.as_bool()).unwrap_or(false),
                            v.get("text")
                                .and_then(|x| x.as_str())
                                .unwrap_or_default()
                                .to_string(),
                        ),
                        None => (false, String::new()),
                    };
                    callback(index, loaded, text);
                },
            );
        }

        /// Where the visible page starts in its section's text — the shell's
        /// last relocation, read on demand: `(section index, UTF-16 offset
        /// into that section's text report)`, the offset `None` when the
        /// page shows no text. The voice starts *here*, not at the section's
        /// top, when the reader asks it to read the open page.
        pub fn page_offset(&self, callback: impl FnOnce(usize, Option<usize>) + 'static) {
            self.view.evaluate_javascript(
                "window.__snbook ? JSON.stringify(window.__snbook.pageOffset()) : ''",
                None,
                None,
                None::<&gtk4::gio::Cancellable>,
                move |result| {
                    let raw = result.map(|v| v.to_str().to_string()).unwrap_or_default();
                    let parsed: Option<(usize, Option<usize>)> = serde_json::from_str::<
                        serde_json::Value,
                    >(&raw)
                    .ok()
                    .and_then(|v| {
                        let index = v.get("index").and_then(|v| v.as_u64())? as usize;
                        let offset = v
                            .get("offset")
                            .and_then(|v| v.as_u64())
                            .map(|v| v as usize);
                        Some((index, offset))
                    });
                    let (index, offset) = parsed.unwrap_or((0, None));
                    callback(index, offset);
                },
            );
        }
    }

// ---------------------------------------------------------------------------
// Serving
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Parsing the reports
// ---------------------------------------------------------------------------

/// Flatten one nesting level of the book's contents. A row at depth `d`
/// whose subitems are non-empty is a group; its children follow at `d + 1`,
/// exactly the shape the reader's sidebar folds.
fn flatten_toc_rows(items: &[serde_json::Value], depth: usize, rows: &mut Vec<BookTocRow>) {
    for item in items {
        let label = item
            .get("label")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let href = item
            .get("href")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let index = item
            .get("index")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        rows.push(BookTocRow {
            label,
            href,
            index,
            depth,
        });
        if let Some(subitems) = item.get("subitems").and_then(|v| v.as_array()) {
            if !subitems.is_empty() {
                flatten_toc_rows(subitems, depth + 1, rows);
            }
        }
    }
}

/// The book report's JSON: `{title, author, language, sectionCount, toc:
/// [{label, href, subitems}]}` — the nested toc is flattened here, depth by
/// nesting, so the reader gets a plain list the sidebar already knows how
/// to fold.
fn parse_book_info(payload: &str) -> Result<BookInfo, String> {
    let value: serde_json::Value =
        serde_json::from_str(payload).map_err(|e| e.to_string())?;
    let mut rows = Vec::new();
    let toc = value
        .get("toc")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    flatten_toc_rows(&toc, 0, &mut rows);
    Ok(BookInfo {
        title: value
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        author: value
            .get("author")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        language: value
            .get("language")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        section_count: value
            .get("sectionCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize,
        toc: rows,
    })
}

/// A relocation's JSON: `{cfi, index, fraction, label, textOffset, sectionCount}`.
fn parse_relocate(payload: &str) -> Result<Relocate, String> {
    let value: serde_json::Value =
        serde_json::from_str(payload).map_err(|e| e.to_string())?;
    Ok(Relocate {
        cfi: value
            .get("cfi")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        index: value
            .get("index")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize,
        fraction: value
            .get("fraction")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0),
        label: value
            .get("label")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        section_count: value
            .get("sectionCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize,
        text_offset: value
            .get("textOffset")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize),
    })
}

/// Percent-encode a path segment: the unreserved set only.
fn epubview_encode_path(path: &str) -> String {
    crate::epubview::encode_path(path)
}

/// Percent-encode a query value: the unreserved set, plus nothing — query
/// values carry prose (titles, CFIs with apostrophes), so everything
/// outside the unreserved set is escaped.
fn epubview_encode_query(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nested_toc_flattens_with_depths() {
        let payload = r#"{"title":"T","author":"A","language":"en","sectionCount":3,
            "toc":[
                {"label":"Part One","href":"0","index":0,"subitems":[
                    {"label":"Chapter 1","href":"1","index":1,"subitems":[]},
                    {"label":"Chapter 2","href":"2","index":2,"subitems":[]}]},
                {"label":"Part Two","href":"2#x","index":2,"subitems":[]}
            ]}"#;
        let info = parse_book_info(payload).expect("parses");
        assert_eq!(info.title, "T");
        assert_eq!(info.section_count, 3);
        let labels: Vec<(String, usize)> = info
            .toc
            .iter()
            .map(|row| (row.label.clone(), row.depth))
            .collect();
        assert_eq!(
            labels,
            vec![
                ("Part One".into(), 0),
                ("Chapter 1".into(), 1),
                ("Chapter 2".into(), 1),
                ("Part Two".into(), 0),
            ]
        );
        assert_eq!(info.toc[1].index, 1);
        assert_eq!(info.toc[3].href, "2#x");
        // A row the shell could not resolve a section for still lands — it
        // opens the book's first section rather than vanishing.
        let payload = r#"{"toc":[{"label":"?","href":"","subitems":[]}]}"#;
        let info = parse_book_info(payload).expect("parses");
        assert_eq!(info.toc[0].index, 0);
    }

    #[test]
    fn a_relocation_parses() {
        let payload = r#"{"cfi":"epubcfi(/6/4)","index":2,"fraction":0.5,
            "label":"Chapter 3","textOffset":512,"sectionCount":10}"#;
        let relocate = parse_relocate(payload).expect("parses");
        assert_eq!(relocate.cfi, "epubcfi(/6/4)");
        assert_eq!(relocate.index, 2);
        assert!((relocate.fraction - 0.5).abs() < 1e-9);
        assert_eq!(relocate.label, "Chapter 3");
        assert_eq!(relocate.section_count, 10);
        assert_eq!(relocate.text_offset, Some(512));
        // A page whose start the shell could not name — a cover image —
        // relocates with the field absent, and the voice falls back to the
        // section's top.
        let payload = r#"{"cfi":"","index":0,"fraction":0,"label":"","sectionCount":3}"#;
        let relocate = parse_relocate(payload).expect("parses");
        assert_eq!(relocate.text_offset, None);
    }

    #[test]
    fn a_broken_report_is_an_error_not_a_panic() {
        assert!(parse_book_info("{not json").is_err());
        assert!(parse_relocate("").is_err());
        // An empty toc is a valid report: a plain text book with no
        // detected headings still opens.
        let info = parse_book_info(r#"{"title":"","toc":[]}"#).expect("parses");
        assert!(info.toc.is_empty());
    }

    #[test]
    fn query_values_encode_everything_but_the_unreserved_set() {
        assert_eq!(epubview_encode_query("plain"), "plain");
        assert_eq!(epubview_encode_query("a b"), "a%20b");
        assert_eq!(epubview_encode_query("a&b=c"), "a%26b%3Dc");
        // An apostrophe in a title must not close the shell's URL string.
        assert_eq!(epubview_encode_query("Gatsby's"), "Gatsby%27s");
    }

    #[test]
    fn js_strings_unescape() {
        // The shell's text reports travel as JSON strings; serde_json does
        // the unescaping, so only that path needs to hold.
        let decoded: String =
            serde_json::from_str(r#""a\nb \u00e9 back\"slash""#).expect("parses");
        assert_eq!(decoded, "a\nb é back\"slash");
    }
}
