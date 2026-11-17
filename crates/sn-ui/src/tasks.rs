//! Worker results, delivered to the UI thread.
//!
//! `Rc<App>` cannot cross a thread, and neither can a widget — so every
//! background job in the reader path ends by sending **plain data** over one
//! channel, whose receiver is polled on the UI thread. The worker never
//! touches the app; the dispatch here is the only place a result becomes UI
//! state.
//!
//! The channel is process-wide and lives for the app's lifetime, so a worker
//! started before the window exists (the startup repair sweep) is simply
//! queued — not lost.

use std::rc::Rc;

use gtk4::glib;

use crate::app::App;

/// What a finished book open carries back: the manifest (plain data), the
/// chapter the reader lands on, its text, and — for a PDF — every page's
/// size in points (so the page scroll can be laid out without another
/// parse) and the book's own link annotations (so its contents page
/// clicks).
pub struct Opened {
    pub manifest: sn_store::books::BookManifest,
    pub index: usize,
    pub text: String,
    pub sizes: Option<Vec<(f64, f64)>>,
    pub links: Vec<sn_doc::pdf::PageLink>,
}

/// What a worker can report back. Every variant that answers a *request*
/// carries the generation counter the request was tagged with, so a result
/// for a superseded request (another book opened, another query typed) is
/// dropped at the dispatch instead of landing on top of newer state.
pub enum Msg {
    /// `reader::open_book`'s worker finished. `Err` is the status line.
    BookOpened { gen: u64, result: Result<Opened, String> },
    /// The worker found the id names an audiobook: the text path stands down
    /// and the UI thread runs the audiobook open from the manifest.
    AudiobookOpened { gen: u64, manifest: sn_store::books::BookManifest },
    /// A page-turn text refill landed.
    PageText { gen: u64, page: usize, text: String },
    /// Speak-from-selection: the pages view's "Speak
    /// from here" — the clicked page's text, extracted on a worker, so the
    /// session can start at that page's first byte. Tagged with the
    /// generation like every other answer.
    SpeakPage { gen: u64, page: usize, text: String },
    /// An in-book search finished.
    SearchDone {
        gen: u64,
        needle: String,
        hits: Vec<(usize, String, usize)>,
    },
    /// A book import finished. `Err` is the failure to show.
    ImportDone { result: Result<sn_store::books::BookManifest, String> },
    /// The startup repair sweep finished, with the ids it rewrote.
    RepairDone { repaired: Vec<String> },
    /// The speech walk's worker finished looking for the first page/chapter
    /// with text at or after the reader's position. `None` means the bounded
    /// walk found nothing speakable.
    SpeakableFound { book_id: String, index: Option<usize> },
    /// The OCR warm-up recognised a page: `done` pages recognised of the
    /// `total` this walk covers, `page` (1-based) just finished. The reader
    /// shows it, so a long recognition is a visible process rather than a
    /// silent thread.
    OcrProgress { book_id: String, done: usize, total: usize, page: usize },
    /// The OCR warm-up finished: `recognised` pages newly read. The reader
    /// puts the ordinary page status back.
    OcrDone { book_id: String, recognised: usize },
    /// A JEX import from the settings Data page finished. `Ok` carries the
    /// summary line, `Err` the failure.
    //  the JEX interchange UI (settings Data page).
    JexImportDone { result: Result<String, String> },
    /// A JEX export from the settings Data page finished.
    JexExportDone { result: Result<String, String> },
}

type Channel = (
    std::sync::mpsc::Sender<Msg>,
    std::sync::Arc<std::sync::Mutex<std::sync::mpsc::Receiver<Msg>>>,
);

static CHANNEL: std::sync::OnceLock<Channel> = std::sync::OnceLock::new();

fn channel() -> &'static Channel {
    CHANNEL.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        (tx, std::sync::Arc::new(std::sync::Mutex::new(rx)))
    })
}

/// Run `f` on a worker thread and deliver its message to the UI thread.
///
/// The closure must be `Send` and own everything it touches — no `Rc`, no
/// widgets — which is the whole discipline of this module.
pub fn spawn<F>(f: F)
where
    F: FnOnce() -> Msg + Send + 'static,
{
    let tx = channel().0.clone();
    std::thread::spawn(move || {
        let msg = f();
        // The receiver lives for the app's lifetime; a send can only fail
        // during shutdown, when there is nobody left to tell.
        let _ = tx.send(msg);
    });
}

/// Run `f` on a worker thread with nothing to report back.
///
/// The fire-and-forget work: a manifest write whose result nobody waits for.
/// Send a message from anywhere on the worker side.
///
///  authored from the call sites (reader.rs
/// `tasks::report(tasks::Msg::OcrProgress { .. })` and the module's own
/// channel design — `spawn` sends through the same sender); the final
/// tasks.rs body carrying it did not survive.
pub fn report(msg: Msg) {
    let _ = channel().0.send(msg);
}

pub fn spawn_detached<F>(f: F)
where
    F: FnOnce() + Send + 'static,
{
    std::thread::spawn(f);
}

/// Poll the channel on the UI thread. Called once, from window construction,
/// before any pending opens are served.
///
/// The poll is cheap when nothing is in flight: one `try_recv` every 40 ms.
pub fn attach(app: &Rc<App>) {
    let app = app.clone();
    let rx = std::sync::Arc::clone(&channel().1);
    glib::timeout_add_local(std::time::Duration::from_millis(40), move || {
        let mut landed = Vec::new();
        {
            let rx = rx.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            loop {
                match rx.try_recv() {
                    Ok(msg) => landed.push(msg),
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                }
            }
        }
        for msg in landed {
            dispatch(&app, msg);
        }
        glib::ControlFlow::Continue
    });
}

/// One result, one pane. This is the only place worker data becomes state.
fn dispatch(app: &Rc<App>, msg: Msg) {
    match msg {
        Msg::BookOpened { gen, result } => crate::reader::on_book_opened(app, gen, result),
        Msg::AudiobookOpened { gen, manifest } => {
            crate::reader::on_audiobook_opened(app, gen, manifest)
        }
        Msg::PageText { gen, page, text } => crate::reader::on_page_text(app, gen, page, text),
        Msg::SpeakPage { gen, page, text } => {
            crate::speaksel::on_speak_page_text(app, gen, page, text)
        }
        Msg::SearchDone { gen, needle, hits } => {
            crate::reader::on_search_done(app, gen, &needle, hits)
        }
        Msg::ImportDone { result } => crate::reader::on_import_done(app, result),
        Msg::RepairDone { repaired } => crate::reader::on_repair_done(app, repaired),
        Msg::SpeakableFound { book_id, index } => {
            crate::player::on_first_speakable_found(app, &book_id, index)
        }
        Msg::OcrProgress {
            book_id,
            done,
            total,
            page,
        } => crate::reader::on_ocr_progress(app, &book_id, done, total, page),
        Msg::OcrDone {
            book_id,
            recognised,
        } => crate::reader::on_ocr_done(app, &book_id, recognised),
        Msg::JexImportDone { result } => crate::settings::on_jex_import_done(app, result),
        Msg::JexExportDone { result } => crate::settings::on_jex_export_done(app, result),
    }
}
