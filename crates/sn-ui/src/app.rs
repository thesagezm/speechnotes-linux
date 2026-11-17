//! Application state shared by every pane.

use gtk4::glib;
use gtk4::prelude::EditableExt;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use sn_store::model::{Note, Scope};
use sn_store::{BookmarkStore, NotebooksStore, NotesStore, PrefsStore, SearchIndex};

/// The TTS model manager, owned by the app so every session start goes
/// through its single-resident path: the picker's change, the speed
/// control's restart and `--play-note` all hand `start_session` the same
/// manager, and the manager guarantees the old engine's model pool is
/// dropped before the new one is opened. UI-thread-only by construction —
/// nothing in sn-ui touches it off the main loop.
pub type SharedModelManager = std::sync::Mutex<sn_tts::manager::ModelManager>;

/// BookDrop's lifecycle: the LocalSend receiver, its listener thread, and
/// what the settings page polls for status. The wire protocol lives in
/// `sn_net::bookdrop`; this is only the running of it inside the app.
pub type SharedBookDrop = std::sync::Arc<crate::bookdrop::Service>;

/// Editor debounce. The store's own save debounce sits behind this, so a burst
/// of typing costs one enqueue and nothing else.
pub const EDIT_DEBOUNCE_MS: u64 = 300;

/// Auto-resume window: the newest note bookmark inside this is offered.
pub const RESUME_WINDOW: std::time::Duration = std::time::Duration::from_secs(6 * 3600);

/// How the app is identified to GApplication and the desktop entry.
pub const APP_ID: &str = "io.github.thesagezm.Speechnotes";

/// Which view the sidebar has selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum View {
    Notes(Scope),
    Bin,
    Books,
}

/// Everything the panes share. One `Rc`, cloned into every signal closure.
///
/// All UI-thread state uses `RefCell`; the stores behind it are `Arc` so a
/// later batch can take one to a playback thread.
pub struct App {
    pub notes: Arc<NotesStore>,
    pub notebooks: Arc<NotebooksStore>,
    pub prefs: Arc<PrefsStore>,
    pub bookmarks: Arc<BookmarkStore>,

    /// Folded text per note, kept across keystrokes. UI-thread owned, so a
    /// `RefCell` is enough — and cheaper than a lock on the hot path.
    pub index: RefCell<SearchIndex>,
    /// The memoisation key of the rendered list: when the query has not changed
    /// and the library version has not moved, the list is not rebuilt.
    pub list_key: RefCell<String>,
    pub view: RefCell<View>,
    /// The note open in the editor.
    pub open_note: RefCell<Option<String>>,
    /// Which note's text the editor buffer currently holds. The buffer is the
    /// source of truth for that note while it stays open — re-filling it on
    /// every refresh reset the caret and ate uncommitted keystrokes — so it
    /// is refilled only when a different note is shown.
    pub buffer_note: RefCell<Option<String>>,
    /// The note whose buffer changed and has not been committed yet. Only the
    /// id is stored: the text is read from the buffer once, at commit time.
    /// Capturing the full text on every keystroke made each key an O(note)
    /// copy and allocation, which a long note turned into visible lag.
    pub pending_edit: RefCell<Option<String>>,
    /// The note whose title entry changed and has not been committed yet.
    /// Title keystrokes used to rebuild the whole notes list per key.
    pub pending_title: RefCell<Option<String>>,
    pub pending_source: RefCell<Option<glib::SourceId>>,

    /// True when the open page — the note editor, its preview, or the book
    /// reader — fills the whole window: both side columns hidden. The pane
    /// positions they were hidden at are remembered in `focus_saved` so the
    /// toggle is round-trip safe.
    pub focus_mode: Cell<bool>,
    /// (sidebar position, list position) captured when focus mode turned on.
    pub focus_saved: Cell<(i32, i32)>,
    /// The floating transport's hide timer, armed every time fill-the-window
    /// sees the mouse move. One at a time: re-arming replaces the old one.
    pub float_source: RefCell<Option<glib::SourceId>>,

    /// Fill-the-window's chrome-hide timer: armed when the mode turns on,
    /// hides the chrome rows once the fade has landed. Cancelled when the
    /// mode leaves — an exit inside the fade window must not let a timer
    /// aimed at the mode's own chrome hide it out of the restored layout
    /// (the transport bar vanishing after a quick F11 tap).
    pub focus_hide_source: RefCell<Option<glib::SourceId>>,

    /// The contents sidebar's state — the book its rows belong to, the
    /// rows, the groups. On the App, not in `Widgets`: `Widgets` derives
    /// `Clone` and a cloned `RefCell` is a **deep copy**, so a write through
    /// `app.widgets.borrow().clone()` would land in a throwaway cell (the
    /// old `toc_list` marking lost its list the same way). Widget handles
    /// — the revealer, the rows' container — stay in `ReaderWidgets` as
    /// plain fields; cloning shares the widgets, never the state.
    pub toc_sidebar: RefCell<crate::reader::TocSidebar>,

    /// Widgets the panes need to reach from anywhere. Set once at build time.
    pub widgets: RefCell<Option<crate::window::Widgets>>,

    /// True while the editor is being filled programmatically, so a fill does
    /// not count as an edit. `Cell` because only the UI thread touches it.
    pub editor_suppress: Cell<bool>,
    /// True while the list is rebuilding, so restoring a selection does not
    /// re-trigger `open_note`.
    pub selection_suppress: Cell<bool>,
    /// Note id per list row index. Rows and this vector are rebuilt together.
    pub row_ids: RefCell<Vec<String>>,
    /// The note text the preview currently shows. Rendering a document is a
    /// parse plus a widget tree, so it is skipped when the text has not moved
    /// — the list rebuilds on every keystroke-debounced save, and a large note
    /// would otherwise be re-rendered for nothing.
    pub preview_text: RefCell<String>,
    /// The id of the note whose widgets the preview holds. Images resolve
    /// per note, so the memo needs the id as well as the text: two notes with
    /// identical text must not share a rendered pane.
    pub preview_note: RefCell<String>,
    /// True when the editor is showing the rendered preview instead of the
    /// source text.
    pub preview: Cell<bool>,
    /// Remote image URLs this run has already asked the network for, successes
    /// and failures alike. The preview records what it cannot show rather than
    /// fetching on the UI thread, and a redraw after a fetch would otherwise
    /// re-queue the URLs that failed — one round trip per picture per run, and
    /// a re-render never becomes a retry storm. Shared with every
    /// [`crate::render::RenderCtx`] through the same `Rc`, so it outlives a
    /// single render pass.
    pub remote_seen: Rc<RefCell<std::collections::HashSet<String>>>,

    /// Remote image URLs whose fetch failed this run. The fetch poll fills
    /// it as the workers report, the next render pass reads it through
    /// [`crate::render::RenderCtx::failed_remote`], and the spinner a dead
    /// link was showing becomes the failure placeholder. Without this set
    /// the placeholder would spin forever — the Android build's known bug,
    /// deliberately not copied.
    pub remote_failed: Rc<RefCell<std::collections::HashSet<String>>>,

    // ---- playback (Batch 5) ---------------------------------------------
    /// The live TTS session. The pipeline runs on its own threads; the UI
    /// thread polls it from the player's tick. `None` between runs.
    pub tts: RefCell<Option<Arc<sn_tts::SessionCore>>>,
    /// The note the session is speaking.
    pub tts_note: RefCell<Option<String>>,
    /// True when the session is speaking a book chapter rather than a note —
    /// it decides which bookmark key the tick writes.
    pub tts_book: Cell<bool>,
    /// The text the session was started against — the plan's byte domain.
    /// Kept so a resume can re-plan and a bookmark carries a valid hash.
    pub tts_text: RefCell<String>,
    /// (plan epoch, chunk boundaries). Re-fetched when the session resumes
    /// and re-plans; epochs keep the cache honest.
    pub tts_plan: RefCell<(u64, Vec<sn_format::chunker::Chunk>)>,
    /// The player's poll source.
    pub tts_tick: RefCell<Option<glib::SourceId>>,
    /// The highlighted (start, end) GTK char range; (0, 0) is none.
    pub tts_highlight: Cell<(u32, u32)>,
    /// djb2 of `tts_text`, to detect mid-playback edits (highlight then
    /// stands down rather than pointing at moved text).
    pub tts_hash: Cell<i64>,
    /// Last frontier a bookmark was written from, so the store is not
    /// hammered with unchanged positions.
    pub tts_saved_frontier: Cell<usize>,
    /// The error count the tick last announced on the status line, so a
    /// failed chunk is said once in words instead of every poll.
    pub tts_errors_shown: Cell<usize>,
    /// A frontier a stopped session should be restarted from (speed change,
    /// restart button) — served on the tick, never inside a signal handler.
    pub tts_restart: Cell<Option<usize>>,
    /// The book the reader has open, where in it the reader stands, and the
    /// open chapter's text. UI-thread owned, like the note panes' state.
    pub reader: RefCell<crate::reader::ReaderState>,
    /// The live audiobook session, when an audiobook is playing.
    pub abook: RefCell<Option<crate::abook::AbookSession>>,
    /// The audiobook player's poll source.
    pub abook_tick: RefCell<Option<glib::SourceId>>,
    // Fields abook.rs reads (`app.abook_scrub.get()`,
    // `app.abook_save_last.get()/.set(..)`); types are pinned by those call
    // sites.
    /// True while the audiobook scrubber is being dragged, so a tick does
    /// not fight the listener's hand.
    pub abook_scrub: Cell<bool>,
    // Three more fields reader.rs reads; types pinned by its call sites.
    /// The OCR warm-up's stop flag; set on every PDF open so the previous
    /// book's walk stands down.
    pub ocr_warmup_stop: Cell<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// The trailing manifest write for a PDF page turn, debounced 1.2 s.
    pub pdf_save_source: RefCell<Option<glib::SourceId>>,
    /// The trailing manifest write for a book-view relocation (the engine's
    /// CFI position), debounced the same 1.2 s.
    pub book_save_source: RefCell<Option<glib::SourceId>>,
    /// The in-book search's debounce source.
    pub search_source: RefCell<Option<glib::SourceId>>,
    //  field the original window.rs reads (`app.shelf_mode.get()`);
    // type pinned by that call site.
    /// The shelf's display mode, an index into the window's SHELF_MODES.
    pub shelf_mode: Cell<u8>,
    //  field the original window.rs reads (`app.rail_saved.get()/.set()`);
    // type pinned by those call sites (the two pane widths).
    /// The panes' widths, saved while a rail is collapsed so it can expand back.
    pub rail_saved: Cell<(i32, i32)>,
    /// The note count the list header shows, from the last query that ran.
    //  field the original window.rs reads (`app.list_count.get()/.set()`).
    pub list_count: Cell<usize>,
    /// When the audiobook position last hit the manifest, for the save
    /// coalescing window `abook::save_position` applies.
    pub abook_save_last: Cell<Option<std::time::Instant>>,
    /// When the sleep timer fires, `None` when it is off.
    pub abook_sleep: RefCell<Option<std::time::Instant>>,
    /// The rate the transport's speed spin is displaying, so a programmatic
    /// set is not mistaken for the listener turning the dial.
    pub shown_speed: std::cell::Cell<f64>,
    /// The library, memoised: `BooksStore::all()` reads and JSON-parses
    /// every manifest on the box, and the list rebuild asked for it three
    /// times per keystroke in the search box. Invalidated by import/delete.
    pub library: RefCell<Option<Vec<sn_store::books::Book>>>,
    /// True when `library` must be re-read (an import or delete happened).
    pub library_dirty: Cell<bool>,
    /// The model manager: catalog, install state, and the one resident
    /// engine. Mutex (not RefCell) so a future download thread can report
    /// progress without a UI-thread borrow held across it.
    pub models: Arc<SharedModelManager>,
    /// BookDrop, running or waiting to. Built in `App::open` so the CLI's
    /// windowed paths (`--open-book`) receive transfers too, and started
    /// right after the first frame when the pref says so.
    pub bookdrop: SharedBookDrop,
}

    impl App {
    /// The active notes, in the store's order.
    // Carved original: verbatim from the app.rs original 2df682699779; the
    // final app.rs copy did not survive.
    pub fn active_notes(&self) -> Vec<Note> {
        self.notes.active()
    }

    pub fn open() -> std::io::Result<Rc<Self>> {
        sn_core::paths::ensure_directories()?;
        // One library sweep before the shelf is built: a manifest written
        // by the pre-sanitiser m4b reader carries rows of raw container
        // bytes as chapter titles, and nothing re-parses the file afterwards,
        // so without this pass that book's contents reads as garbage for
        // ever. A PDF imported before poppler rendering existed has no cover
        // drawn, and the shelf shows it as a placeholder for ever. It costs
        // one comparison per TOC row and rewrites nothing when there is
        // nothing to fix.
        if let Ok(repaired) = sn_store::books::BooksStore::repair_library() {
            for id in repaired {
                tracing::info!(book = %id, "repaired a library manifest");
            }
        }
        let notes = Arc::new(NotesStore::open()?);
        let notebooks = Arc::new(NotebooksStore::open()?);
        let prefs = Arc::new(PrefsStore::open()?);
        heal_voice_prefs(&prefs);
        let bookmarks = Arc::new(BookmarkStore::open()?);
        let index = SearchIndex::build(&notes.active());
        // Open on the section the user last had open, not on whatever
        // `activeNotebookScope` holds — that key was written once (by this
        // app or a sibling) and nothing updated it afterwards, so every
        // launch landed on the same section. A notebook section that has
        // since been deleted falls back to the scope key's own reading.
        let initial_view = {
            let p = prefs.get();
            match p.last_view.as_str() {
                "bin" => View::Bin,
                "books" => View::Books,
                "all" => View::Notes(Scope::All),
                "unfiled" => View::Notes(Scope::Unfiled),
                other => match other.strip_prefix("notebook:") {
                    Some(id) if notebooks.all().iter().any(|nb| nb.id == id) => {
                        View::Notes(Scope::Notebook(id.to_string()))
                    }
                    _ => View::Notes(p.scope()),
                },
            }
        };
        // BookDrop is built here, not later: the CLI's windowed paths
        // (`--open-book`) run the same app, and a transfer that lands while
        // a book is open must reach the library just the same.
        let bookdrop = crate::bookdrop::Service::with_stores(
            Some(&prefs),
            notes.clone(),
            notebooks.clone(),
        );
        // Persist the fingerprint this install generated: LocalSend peers
        // are identified by it, and a new one every launch would make this
        // machine look like a different device each time.
        let fingerprint = bookdrop.fingerprint();
        if prefs.get().book_drop_fingerprint != fingerprint {
            prefs.update(|p| p.book_drop_fingerprint = fingerprint);
        }
        Ok(Rc::new(Self {
            notes,
            notebooks,
            prefs,
            bookmarks,
            //  the struct declares `float_source` but this version's
            // initializer lost the line.
            float_source: RefCell::new(None),
            focus_hide_source: RefCell::new(None),
            index: RefCell::new(index),
            list_key: RefCell::new(String::new()),
            view: RefCell::new(initial_view),
            open_note: RefCell::new(None),
            buffer_note: RefCell::new(None),
            pending_edit: RefCell::new(None),
            pending_title: RefCell::new(None),
            pending_source: RefCell::new(None),
            widgets: RefCell::new(None),
            toc_sidebar: RefCell::new(crate::reader::TocSidebar::default()),
            focus_mode: Cell::new(false),
            focus_saved: Cell::new((235, 400)),
            editor_suppress: Cell::new(false),
            selection_suppress: Cell::new(false),
            row_ids: RefCell::new(Vec::new()),
            preview_text: RefCell::new(String::new()),
            preview_note: RefCell::new(String::new()),
            preview: Cell::new(false),
            remote_seen: Rc::new(RefCell::new(std::collections::HashSet::new())),
            remote_failed: Rc::new(RefCell::new(std::collections::HashSet::new())),
            tts: RefCell::new(None),
            tts_note: RefCell::new(None),
            tts_book: Cell::new(false),
            tts_text: RefCell::new(String::new()),
            tts_plan: RefCell::new((0, Vec::new())),
            tts_tick: RefCell::new(None),
            tts_highlight: Cell::new((0, 0)),
            tts_hash: Cell::new(0),
            tts_saved_frontier: Cell::new(usize::MAX),
            tts_errors_shown: Cell::new(0),
            tts_restart: Cell::new(None),
            reader: RefCell::new(crate::reader::ReaderState::default()),
            abook: RefCell::new(None),
            abook_tick: RefCell::new(None),
            abook_scrub: Cell::new(false),
            abook_save_last: Cell::new(None),
            ocr_warmup_stop: Cell::new(std::sync::Arc::new(
                std::sync::atomic::AtomicBool::new(false),
            )),
            pdf_save_source: RefCell::new(None),
            book_save_source: RefCell::new(None),
            search_source: RefCell::new(None),
            shelf_mode: Cell::new(0),
            rail_saved: Cell::new((235, 400)),
            list_count: Cell::new(0),
            abook_sleep: RefCell::new(None),
            shown_speed: std::cell::Cell::new(0.0),
            library: RefCell::new(None),
            library_dirty: Cell::new(true),
            models: Arc::new(std::sync::Mutex::new(sn_tts::manager::ModelManager::new())),
            bookdrop,
        }))
    }

    pub fn flush_all(&self) {
        self.notes.flush();
        self.notebooks.flush();
        self.prefs.flush();
        self.bookmarks.flush();
    }    pub fn current_scope(&self) -> Scope {
        match &*self.view.borrow() {
            View::Notes(scope) => scope.clone(),
            View::Bin | View::Books => Scope::All,
        }
    }

    /// The visible notes for the current view, against the folded index.
    pub fn visible_notes(&self) -> Vec<Note> {
        match &*self.view.borrow() {
            View::Bin => self.notes.binned_active(),
            // The books shelf shows the library, not the notes query; the
            // pane is rebuilt from the book store itself.
            View::Books => Vec::new(),
            View::Notes(scope) => {
                let prefs = self.prefs.get();
                let q = sn_store::Query {
                    scope: scope.clone(),
                    search: self.search_text(),
                    sort: prefs.sort_order(),
                };
                let notes = self.notes.active();
                self.index.borrow_mut().sync(&notes);
                q.run_indexed(&notes, &self.index.borrow())
            }
        }
    }

    pub fn search_text(&self) -> String {
        self.widgets
            .borrow()
            .as_ref()
            .map(|w| w.search.text().to_string())
            .unwrap_or_default()
    }

    /// The open note, if it still exists (it may have been purged under us).
    pub fn open_note(&self) -> Option<Note> {
        self.open_note
            .borrow()
            .as_ref()
            .and_then(|id| self.notes.get(id))
    }
}

/// Drop the voice-pref entries that name a voice their engine does not
/// offer, once per launch, before any picker is built.
///
/// A picker bug (the unguarded rebuilds, fixed 2026-11-13) wrote a phonon
/// voice id under the espeak key; the espeak engine then failed every
/// chunk at synthesis and the listener heard the skip-beep through whole
/// sessions. `build_engine` now falls back to an engine's default for such
/// a voice at build time — this pass keeps the pickers honest too, so the
/// poisoned row never shows and the file heals instead of carrying the
/// damage until the user happens to overwrite it. An engine whose set is
/// not installed keeps its entry: its list is empty because its files are
/// gone, and a reinstall must not lose the user's voice. An engine the
/// family no longer has at all (the `is_known_engine` rule — soprano's
/// precedent) loses its entry: there is nothing to
/// reinstall into, and the key would sit in the file forever.
fn heal_voice_prefs(prefs: &PrefsStore) {
    use sn_tts::manager::{InstallState, ModelManager};
    let entries: Vec<(String, String)> = prefs
        .get()
        .voices_by_engine
        .iter()
        .map(|(engine, voice)| (engine.clone(), voice.clone()))
        .collect();
    let mut healed: Vec<String> = Vec::new();
    for (engine, voice) in entries {
        // An engine the family no longer has at all (the `is_known_engine`
        // rule — soprano's precedent) loses its entry: there is nothing to
        // reinstall into, and the key would sit in the file forever.
        if !sn_store::model::is_known_engine(&engine) {
            healed.push(engine);
            continue;
        }
        let engine_known = ModelManager::set_for(&engine).is_some()
            || engine == "espeak"
            || engine == "pico";
        let engine_installed = match engine.as_str() {
            "espeak" | "pico" => true,
            other => ModelManager::state(other) == InstallState::Installed,
        };
        if !engine_known || !engine_installed {
            continue;
        }
        let voice_known = ModelManager::voices(&engine)
            .iter()
            .any(|v| v.code == voice);
        if !voice_known {
            healed.push(engine);
        }
    }
    if healed.is_empty() {
        return;
    }
    tracing::warn!(
        engines = ?healed,
        "voice prefs name voices their engines do not offer; dropping them"
    );
    prefs.update(|p| {
        for engine in &healed {
            p.voices_by_engine.remove(engine);
        }
    });
}
