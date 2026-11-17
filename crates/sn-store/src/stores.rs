//! Preferences, notebooks and bookmarks: the three small stores.
//!
//! They share one shape — an in-memory value, a writer thread, a load report —
//! so each store is short and the behaviour lives in one place.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use serde::{Deserialize, Serialize};
use sn_core::atomic::{DebouncedWriter, SharedWriter, WriterThread};
use sn_core::paths;
use sn_core::AppleDate;
use sn_core::util;

use crate::model::{
    book_key, note_key, Accent, Appearance, PlaybackBookmark, Scope, SortOrder, ACCENTS,
};
use crate::notes::LoadReport;

/// How long a preference change waits before it is written. Matches the Swift
/// port's 400 ms so a switch flipped twice in a burst costs one write.
pub const PREFS_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(400);

// ---------------------------------------------------------------------------
// Prefs
// ---------------------------------------------------------------------------

/// Every preference, with the defaults the other apps use.
///
/// Unknown keys from a newer or foreign `prefs.json` are *preserved on write*,
/// not dropped: a value the Rust app does not understand yet must survive a
/// round trip, or switching apps would reset someone's setting.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Prefs {
    #[serde(default)]
    pub has_onboarded: bool,
    #[serde(default)]
    pub active_notebook_scope: String,
    #[serde(default = "default_sort")]
    pub notes_sort_order: String,
    #[serde(default)]
    pub render_markdown: bool,
    #[serde(default = "default_true")]
    pub read_along_enabled: bool,
    #[serde(default = "default_engine")]
    pub engine_kind: String,
    /// Legacy espeak-only voice field, kept because iOS writes it.
    #[serde(default)]
    pub voice: String,
    /// Per-engine voice, the modern form. Keys are engine names.
    #[serde(default)]
    pub voices_by_engine: std::collections::HashMap<String, String>,
    /// The speech language the voice lists filter to: `""` = all
    /// languages. A locale prefix (`"en"`) as the voice locales name it,
    /// or the literal `"characters"` group. Serde-defaulted so an older
    /// `prefs.json` reads as All rather than failing to parse.
    #[serde(default)]
    pub speech_language: String,
    //  a missing rateMultiplier must read as 1.0, not serde's 0.0 —
    // clamp() then pushed 0.0 to the 0.5 floor, so an older prefs.json
    // without the key made every voice speak at half speed.
    #[serde(default = "default_rate_multiplier")]
    pub rate_multiplier: f64,
    #[serde(default)]
    pub appearance: String,
    #[serde(default = "default_accent")]
    pub accent_choice: String,
    #[serde(default)]
    pub reader_text_scale: f64,
    /// Reading line spacing, a multiplier on the font's own line height. 1.55
    /// is the value theme.css shipped with, which is why it is the default.
    #[serde(default = "default_line_height")]
    pub reader_line_height: f64,
    /// Space between the book reader's paragraphs, in pixels. The reader's
    /// content box spacing used to be a literal 10.
    #[serde(default = "default_paragraph_gap")]
    pub reader_paragraph_spacing: f64,
    // --- the book reader's own appearance ---------------------------------
    // The EPUB viewer shows the book as the book: the publisher's own XHTML
    // in a web engine, with the publisher's stylesheet applying. These are
    // the knobs laid *over* that, in calibre's names and semantics — the
    // same set the iOS app's reader has, so a choice made on the phone
    // means the same thing here.
    /// The colour scheme: `"light"` | `"sepia"` | `"dark"` | `"trueBlack"`.
    /// The iOS/DATA-FORMAT spelling, which is also the one the other apps
    /// write. Calibre's own names (`system`, `white`, `black`,
    /// `sepia-light`, `sepia-dark`) are not used: the family shares one
    /// vocabulary.
    #[serde(default = "default_reader_theme")]
    pub reader_theme: String,
    /// Which font the reader asks for: `""` = the book's own, else
    /// `"serif"`, `"sans"` or `"mono"`. Calibre's
    /// `standalone_font_settings.standard_font`.
    #[serde(default)]
    pub reader_font: String,
    /// A font family name, overriding the preset above. Empty = the
    /// engine's default.
    #[serde(default)]
    pub reader_font_family: String,
    /// The base font size in pixels. Calibre's `base_font_size`.
    #[serde(default = "default_reader_font_size")]
    pub reader_font_size: f64,
    /// The smallest font the engine will use, in pixels. Calibre's
    /// `standalone_font_settings.minimum_font_size`.
    #[serde(default = "default_reader_min_font_size")]
    pub reader_min_font_size: f64,
    /// Extra space between letters, in pixels. Zero = none.
    #[serde(default)]
    pub reader_letter_spacing: f64,
    /// The margins around the text, in pixels.
    #[serde(default = "default_reader_margin_v")]
    pub reader_margin_top: f64,
    #[serde(default = "default_reader_margin_v")]
    pub reader_margin_bottom: f64,
    #[serde(default = "default_reader_margin_h")]
    pub reader_margin_left: f64,
    #[serde(default = "default_reader_margin_h")]
    pub reader_margin_right: f64,
    /// The widest the text column may be, in pixels. Zero = as wide as the
    /// window. Calibre's `max_text_width`.
    #[serde(default)]
    pub reader_max_width: f64,
    /// How the book flows: `"flow"` (a normal scroll) or `"paged"` (column
    /// pagination, one screen at a time). Calibre's `read_mode`.
    #[serde(default = "default_reader_flow")]
    pub reader_flow: String,
    /// How many columns a paged spread has. 1–3.
    #[serde(default = "default_reader_columns")]
    pub reader_columns: u32,
    /// Whether the reader's colour scheme replaces the book's own colours:
    /// `"never"` (the book's colours survive), `"dark"` (only in dark mode)
    /// or `"always"`. Calibre's `override_book_colors`.
    ///
    /// The default is `"never"`: preserving the publisher's design is the
    /// point of showing the book in a web engine, and this is the escape
    /// hatch for a book whose colours are unreadable.
    #[serde(default = "default_reader_override")]
    pub reader_override_colors: String,
    /// Whether the book's own stylesheet wins. On by default: the reader's
    /// margins and spacing are added around the book, never over it.
    #[serde(default = "default_true")]
    pub reader_respect_styles: bool,
    /// Same two knobs for the note preview, whose defaults are the values
    /// theme.css and the preview column's spacing carried.
    #[serde(default = "default_line_height")]
    pub note_line_height: f64,
    #[serde(default = "default_paragraph_gap")]
    pub note_paragraph_spacing: f64,
    /// How the books view shows its books: `"cards"` (fixed-size cover cards
    /// on a shelf) or `"list"` (rows, like the notes list).
    #[serde(default = "default_shelf_view")]
    pub book_shelf_view: String,
    /// The most columns the cards shelf may use, 1–6. Cards are fixed-width,
    /// so a narrow pane reflows *below* this — the number is the ceiling the
    /// user chose, not a width the cards are stretched to fill.
    #[serde(default = "default_shelf_columns")]
    pub book_shelf_columns: u32,
    /// The book list's row size, an ordinal into the UI's size table
    /// (0 compact … 3 large). The rows carry cover thumbnails since the
    /// shelf's appearance layer, and thumbnails + two text lines have a
    /// height the user should be able to choose — the cards shelf has its
    /// size cycle; list mode had nothing.
    #[serde(default = "default_list_size")]
    //  field renamed out of the method's way — the original window.rs
    // calls `prefs.book_list_size()` (a method); serde keeps the wire name.
    pub book_list_size_ordinal: u32,
    //  same missing-key class as rate_multiplier below — 1.0, not the
    // 0.5 that clamp() makes of serde's 0.0.
    #[serde(default = "default_audio_rate")]
    pub audio_rate: f64,
    /// Sleep timer in minutes; 0 means off.
    #[serde(default)]
    pub sleep_timer_minutes: u32,
    #[serde(default)]
    pub book_drop_enabled: bool,
    /// Receive-side access code (iOS key `bookDropPin`, same value). Empty
    /// means no code: the LAN peer needs none.
    #[serde(default)]
    pub book_drop_pin: String,
    /// Whether an incoming transfer is accepted without asking (iOS key
    /// `bookDropAutoAccept`, default true — the receiver as a whole is off
    /// until `bookDropEnabled`, so "on by default" costs nothing).
    #[serde(default = "default_true")]
    pub book_drop_auto_accept: bool,
    /// Stable per-install id for LocalSend handshakes, so this machine never
    /// discovers itself as a peer (iOS key `bookDropFingerprint`).
    #[serde(default)]
    pub book_drop_fingerprint: String,
    /// The access code this device OFFERS when sending to a peer (iOS key
    /// `bookDropSendPin`). Deliberately a different key from
    /// `book_drop_pin`: a machine can require one code from others and send
    /// to a peer whose code is different — one key would cross-contaminate
    /// them.
    #[serde(default)]
    pub book_drop_send_pin: String,
    /// How a slide deck fills the reader: `"width"` (the slide spans the
    /// window's width) or `"page"` (the whole slide visible). The zoom
    /// buttons step a manual zoom, which is deliberately not stored — a
    /// zoom level is a per-reading gesture, a fit mode is a preference.
    #[serde(default = "default_slides_fit")]
    pub slides_fit: String,
    /// Whether a click steps to the next slide in presentation mode
    /// (fill-the-window over a deck). On: the projector-clicker default.
    #[serde(default = "default_true")]
    pub slides_click_advance: bool,
    /// Whether the voice walks to the next slide with something to say when
    /// a slide finishes. On: a deck is a lecture, not one slide.
    #[serde(default = "default_true")]
    pub slides_auto_advance: bool,
    /// What the gear button opens: a page id from `settings::Page`, or a
    /// newer app's page this one does not know — preserved verbatim by the
    /// `extra` bucket if so, so round-tripping between the family's apps
    /// never eats a selection.
    #[serde(default = "default_settings_page")]
    pub settings_page: String,
    /// The last sidebar section the user had open, so the window reopens on
    /// it rather than on whatever `activeNotebookScope` happens to hold.
    /// One of `"all"`, `"unfiled"`, `"bin"`, `"books"`, or
    /// `"notebook:<id>"`; empty means "never chosen" and falls back to the
    /// scope key, which is what the family's other apps write.
    #[serde(default)]
    pub last_view: String,
    /// Keys this app does not know yet. Written back verbatim.
    #[serde(flatten, default)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

fn default_sort() -> String {
    "edited".to_string()
}

fn default_true() -> bool {
    true
}

fn default_engine() -> String {
    "espeak".to_string()
}

fn default_rate_multiplier() -> f64 {
    1.0
}

fn default_audio_rate() -> f64 {
    1.0
}

fn default_accent() -> String {
    "blue".to_string()
}

fn default_settings_page() -> String {
    "appearance".to_string()
}

fn default_line_height() -> f64 {
    1.55
}

fn default_paragraph_gap() -> f64 {
    10.0
}

fn default_shelf_view() -> String {
    "tiles".to_string()
}

fn default_shelf_columns() -> u32 {
    4
}

fn default_list_size() -> u32 {
    1
}

fn default_reader_theme() -> String {
    "light".to_string()
}

fn default_reader_font_size() -> f64 {
    18.0
}

fn default_reader_min_font_size() -> f64 {
    10.0
}

fn default_reader_margin_v() -> f64 {
    16.0
}

fn default_reader_margin_h() -> f64 {
    24.0
}

fn default_reader_flow() -> String {
    "flow".to_string()
}

fn default_reader_columns() -> u32 {
    1
}

fn default_reader_override() -> String {
    "never".to_string()
}

fn default_slides_fit() -> String {
    "width".to_string()
}

impl Default for Prefs {
    /// The defaults the other apps write, not the zero values a derived
    /// `Default` would produce: an unset `notesSortOrder` must read as
    /// `"edited"`, because that is what a file with the key missing means to
    /// the iOS app.
    fn default() -> Self {
        Self {
            has_onboarded: false,
            active_notebook_scope: String::new(),
            notes_sort_order: default_sort(),
            render_markdown: false,
            read_along_enabled: default_true(),
            engine_kind: default_engine(),
            voice: String::new(),
            voices_by_engine: std::collections::HashMap::new(),
            speech_language: String::new(),
            rate_multiplier: 1.0,
            appearance: Appearance::default().to_pref().to_string(),
            accent_choice: default_accent(),
            reader_text_scale: 1.0,
            reader_line_height: default_line_height(),
            reader_paragraph_spacing: default_paragraph_gap(),
            reader_theme: default_reader_theme(),
            reader_font: String::new(),
            reader_font_family: String::new(),
            reader_font_size: default_reader_font_size(),
            reader_min_font_size: default_reader_min_font_size(),
            reader_letter_spacing: 0.0,
            reader_margin_top: default_reader_margin_v(),
            reader_margin_bottom: default_reader_margin_v(),
            reader_margin_left: default_reader_margin_h(),
            reader_margin_right: default_reader_margin_h(),
            reader_max_width: 0.0,
            reader_flow: default_reader_flow(),
            reader_columns: 1,
            reader_override_colors: default_reader_override(),
            reader_respect_styles: true,
            book_shelf_view: default_shelf_view(),
            book_shelf_columns: default_shelf_columns(),
            book_list_size_ordinal: default_list_size(),
            note_line_height: default_line_height(),
            note_paragraph_spacing: default_paragraph_gap(),
            audio_rate: 1.0,
            sleep_timer_minutes: 0,
            book_drop_enabled: false,
            book_drop_pin: String::new(),
            book_drop_auto_accept: default_true(),
            book_drop_fingerprint: String::new(),
            book_drop_send_pin: String::new(),
            slides_fit: default_slides_fit(),
            slides_click_advance: default_true(),
            slides_auto_advance: default_true(),
            settings_page: default_settings_page(),
            last_view: String::new(),
            extra: std::collections::HashMap::new(),
        }
    }
}

impl Prefs {
    /// The book list's row size, as the shelf reads it.
    //  the method shape the original window.rs calls
    // (`prefs.book_list_size()`); the reconstructed stores.rs had carried it
    // as a plain field.
    pub fn book_list_size(&self) -> usize {
        self.book_list_size_ordinal as usize
    }


    pub fn load() -> Self {
        let path = paths::prefs_file();
        let (mut prefs, _outcome) =
            sn_core::atomic::load_json_recoverable::<Prefs>(&path);
        prefs.clamp();
        prefs
    }

    /// Clamp numeric prefs into their real ranges.
    ///
    /// A hand-edited or foreign `rateMultiplier` of 900 must not become a voice
    /// racing through a note at nine times speed; the UI sliders bound what they
    /// set, and this bounds what arrives from outside.
    pub fn clamp(&mut self) {
        self.rate_multiplier = util::clamp_f64(self.rate_multiplier, 0.5, 2.0);
        self.audio_rate = util::clamp_f64(self.audio_rate, 0.5, 3.0);
        self.reader_text_scale = util::clamp_f64(self.reader_text_scale, 0.75, 1.5);
        self.reader_line_height = util::clamp_f64(self.reader_line_height, 1.0, 2.2);
        self.reader_paragraph_spacing = util::clamp_f64(self.reader_paragraph_spacing, 0.0, 28.0);
        // The book reader's own knobs, bounded to the ranges the settings
        // page offers. A hand-edited font size of 900 px would otherwise
        // render one letter per line.
        self.reader_font_size = util::clamp_f64(self.reader_font_size, 12.0, 32.0);
        self.reader_min_font_size = util::clamp_f64(self.reader_min_font_size, 8.0, 16.0);
        self.reader_letter_spacing = util::clamp_f64(self.reader_letter_spacing, 0.0, 4.0);
        self.reader_margin_top = util::clamp_f64(self.reader_margin_top, 0.0, 96.0);
        self.reader_margin_bottom = util::clamp_f64(self.reader_margin_bottom, 0.0, 96.0);
        self.reader_margin_left = util::clamp_f64(self.reader_margin_left, 0.0, 96.0);
        self.reader_margin_right = util::clamp_f64(self.reader_margin_right, 0.0, 96.0);
        self.reader_max_width = util::clamp_f64(self.reader_max_width, 0.0, 2000.0);
        self.reader_columns = self.reader_columns.clamp(1, 3);
        // The theme and the two mode strings are from a closed set; an
        // unknown value means "the key was missing" and reads as the
        // default, not as a broken theme.
        if !["light", "sepia", "dark", "trueBlack"].contains(&self.reader_theme.as_str()) {
            self.reader_theme = default_reader_theme();
        }
        if !["", "serif", "sans", "mono"].contains(&self.reader_font.as_str()) {
            self.reader_font = String::new();
        }
        if !["flow", "paged"].contains(&self.reader_flow.as_str()) {
            self.reader_flow = default_reader_flow();
        }
        if !["never", "dark", "always"].contains(&self.reader_override_colors.as_str()) {
            self.reader_override_colors = default_reader_override();
        }
        if !["width", "page"].contains(&self.slides_fit.as_str()) {
            self.slides_fit = default_slides_fit();
        }
        // The shelf's view mode: "cards" was tiles under its old name, and
        // anything unknown reads as tiles too — the default look.
        if !["small", "tiles", "large", "vlarge", "list"].contains(&self.book_shelf_view.as_str())
        {
            self.book_shelf_view = "tiles".to_string();
        }
        self.note_line_height = util::clamp_f64(self.note_line_height, 1.0, 2.2);
        self.note_paragraph_spacing = util::clamp_f64(self.note_paragraph_spacing, 0.0, 28.0);
        if !crate::model::is_known_engine(&self.engine_kind) {
            self.engine_kind = "espeak".to_string();
        }
        // An empty sort or appearance string means "the key was missing", and
        // `from_pref` already maps unknown values to their defaults; normalise
        // here so the in-memory value equals what will be written back.
        self.notes_sort_order = SortOrder::from_pref(&self.notes_sort_order)
            .to_pref()
            .to_string();
        self.appearance = Appearance::from_pref(&self.appearance)
            .to_pref()
            .to_string();
        if !ACCENTS.contains(&self.accent_choice.as_str()) {
            self.accent_choice = "blue".to_string();
        }
        self.sleep_timer_minutes = self.sleep_timer_minutes.min(120);
        // An unknown page id is not clamped away: the settings window
        // remembers `settings_page` as written and falls back to its first
        // page on open, so a page renamed later still lands somewhere sane
        // without this field lying about what was stored.
    }

    pub fn scope(&self) -> Scope {
        Scope::from_pref(&self.active_notebook_scope)
    }

    pub fn set_scope(&mut self, scope: Scope) {
        self.active_notebook_scope = scope.to_pref();
    }

    pub fn sort_order(&self) -> SortOrder {
        SortOrder::from_pref(&self.notes_sort_order)
    }

    pub fn set_sort_order(&mut self, order: SortOrder) {
        self.notes_sort_order = order.to_pref().to_string();
    }

    pub fn appearance(&self) -> Appearance {
        Appearance::from_pref(&self.appearance)
    }

    pub fn set_appearance(&mut self, appearance: Appearance) {
        self.appearance = appearance.to_pref().to_string();
    }

    /// True when the books view shows rows rather than cards. Anything but
    /// the literal `"list"` is cards — the default and what every older
    /// prefs.json implies.
    pub fn book_shelf_is_list(&self) -> bool {
        self.book_shelf_mode() == "list"
    }

    /// The shelf's view mode, normalized to one of the five the UI offers:
    /// `"small" | "tiles" | "large" | "vlarge" | "list"`. Older prefs files
    /// carry `"cards"`, which was the tiles look under its old name; an
    /// unknown value falls back to tiles rather than minting a broken mode.
    pub fn book_shelf_mode(&self) -> &'static str {
        match self.book_shelf_view.as_str() {
            "small" => "small",
            "large" => "large",
            "vlarge" => "vlarge",
            "list" => "list",
            _ => "tiles",
        }
    }

    /// The shelf's card-size mode as the UI's `SHELF_MODES` ordinal
    /// (0=small, 1=tiles, 2=large, 3=vlarge). List mode maps to tiles: the
    /// cycle button is hidden there, so the value is only cosmetic.
    pub fn shelf_mode_ordinal(&self) -> usize {
        match self.book_shelf_mode() {
            "small" => 0,
            "large" => 2,
            "vlarge" => 3,
            _ => 1,
        }
    }

    /// The cards shelf's column ceiling, clamped to the 1–6 range the picker
    /// offers (a hand-edited prefs.json may hold anything).
    pub fn book_shelf_columns(&self) -> u32 {
        self.book_shelf_columns.clamp(1, 6)
    }

    pub fn accent(&self) -> Accent {
        Accent::from_pref(&self.accent_choice)
    }

    /// The voice for an engine, honouring the legacy `voice` field for espeak.
    pub fn voice_for_engine(&self, engine: &str) -> String {
        if let Some(v) = self.voices_by_engine.get(engine) {
            return v.clone();
        }
        if engine == "espeak" {
            return self.voice.clone();
        }
        String::new()
    }

    pub fn set_voice_for_engine(&mut self, engine: &str, voice: &str) {
        self.voices_by_engine
            .insert(engine.to_string(), voice.to_string());
    }
}

/// A `RwLock`-guarded preference set that persists through a writer thread.
pub struct PrefsStore {
    prefs: RwLock<Prefs>,
    path: PathBuf,
    writer: SharedWriter,
    _thread: WriterThread,
}

impl PrefsStore {
    pub fn open() -> std::io::Result<Self> {
        let path = paths::prefs_file();
        let prefs = Prefs::load();
        let (writer, thread) = DebouncedWriter::new(path.clone(), PREFS_DEBOUNCE)?.share();
        Ok(Self {
            prefs: RwLock::new(prefs),
            path,
            writer,
            _thread: thread,
        })
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// A snapshot for reading; cheap because `Prefs` is small.
    pub fn get(&self) -> Prefs {
        self.prefs.read().unwrap().clone()
    }

    /// Apply `f` and persist. The closure runs under the write lock.
    pub fn update<F>(&self, f: F)
    where
        F: FnOnce(&mut Prefs),
    {
        {
            let mut prefs = self.prefs.write().unwrap();
            f(&mut prefs);
            prefs.clamp();
        }
        self.persist();
    }

    fn persist(&self) {
        let prefs = self.prefs.read().unwrap().clone();
        self.writer.schedule(
            serde_json::to_vec_pretty(&prefs)
                .map(|mut b| {
                    b.push(b'\n');
                    b
                })
                .unwrap_or_default(),
        );
    }

    /// Write synchronously. Used on quit, where a debounced 400 ms write is
    /// exactly what the Swift port lost.
    pub fn flush(&self) {
        let prefs = self.prefs.read().unwrap().clone();
        let bytes = serde_json::to_vec_pretty(&prefs)
            .map(|mut b| {
                b.push(b'\n');
                b
            })
            .unwrap_or_default();
        let _ = self.writer.flush();
        self.writer.now(bytes);
        let _ = self.writer.flush();
    }
}

// ---------------------------------------------------------------------------
// Notebooks
// ---------------------------------------------------------------------------

pub struct NotebooksStore {
    notebooks: Arc<RwLock<Vec<crate::model::Notebook>>>,
    writer: SharedWriter,
    _thread: WriterThread,
    load_report: LoadReport,
}

impl NotebooksStore {
    pub fn open() -> std::io::Result<Self> {
        let path = paths::notebooks_file();
        let existed = path.exists();
        let (notebooks, outcome) =
            sn_core::atomic::load_json_recoverable::<Vec<crate::model::Notebook>>(&path);
        let (writer, thread) =
            DebouncedWriter::new(path, PREFS_DEBOUNCE)?.share();
        let store = Self {
            notebooks: Arc::new(RwLock::new(notebooks)),
            writer,
            _thread: thread,
            load_report: LoadReport::from_outcome(&outcome, existed),
        };
        store.sort();
        store.persist();
        Ok(store)
    }

    /// Keep the list ordered by creation time, as the other apps do, so two
    /// apps writing the same library agree on the display order.
    fn sort(&self) {
        self.notebooks
            .write()
            .unwrap()
            .sort_by(|a, b| a.created_at.raw().total_cmp(&b.created_at.raw()));
    }

    pub fn load_report(&self) -> &LoadReport {
        &self.load_report
    }

    pub fn all(&self) -> Vec<crate::model::Notebook> {
        self.notebooks.read().unwrap().clone()
    }

    pub fn len(&self) -> usize {
        self.notebooks.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, id: &str) -> Option<crate::model::Notebook> {
        self.notebooks
            .read()
            .unwrap()
            .iter()
            .find(|n| n.id == id)
            .cloned()
    }

    /// Find by name, case-insensitively. JEX import merges on this.
    pub fn by_name(&self, name: &str) -> Option<crate::model::Notebook> {
        self.notebooks
            .read()
            .unwrap()
            .iter()
            .find(|n| n.same_name_as(name))
            .cloned()
    }

    /// Create a notebook, or return the existing one with that name.
    ///
    /// Case-insensitive uniqueness is a *user-visible* rule: two notebooks
    /// called "Work" and "work" are one notebook to everybody but the file
    /// format, and the other apps treat them as one.
    pub fn create(&self, name: &str) -> crate::model::Notebook {
        let name = name.trim();
        if let Some(existing) = self.by_name(name) {
            return existing;
        }
        let notebook = crate::model::Notebook::new(name);
        self.notebooks.write().unwrap().push(notebook.clone());
        self.sort();
        self.persist();
        notebook
    }

    pub fn rename(&self, id: &str, name: &str) -> Option<crate::model::Notebook> {
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        {
            let mut notebooks = self.notebooks.write().unwrap();
            // Renaming onto another notebook's name would merge them silently;
            // refuse instead.
            if notebooks.iter().any(|n| n.id != id && n.same_name_as(name)) {
                return None;
            }
            let notebook = notebooks.iter_mut().find(|n| n.id == id)?;
            notebook.name = name.to_string();
        }
        self.persist();
        self.get(id)
    }

    /// Delete a notebook. Its notes become Unfiled — the caller does that,
    /// because this store does not own the notes.
    pub fn delete(&self, id: &str) -> bool {
        let before = self.len();
        self.notebooks.write().unwrap().retain(|n| n.id != id);
        let removed = self.len() != before;
        if removed {
            self.persist();
        }
        removed
    }

    /// Insert without re-sorting. Test helper; `create` always sorts.
    pub fn insert_out_of_order(&self, notebook: crate::model::Notebook) {
        self.notebooks.write().unwrap().push(notebook);
        self.sort();
    }

    fn persist(&self) {
        let notebooks = self.notebooks.read().unwrap().clone();
        self.writer.schedule(
            serde_json::to_vec_pretty(&notebooks)
                .map(|mut b| {
                    b.push(b'\n');
                    b
                })
                .unwrap_or_default(),
        );
    }

    pub fn flush(&self) {
        let notebooks = self.notebooks.read().unwrap().clone();
        let bytes = serde_json::to_vec_pretty(&notebooks)
            .map(|mut b| {
                b.push(b'\n');
                b
            })
            .unwrap_or_default();
        let _ = self.writer.flush();
        self.writer.now(bytes);
        let _ = self.writer.flush();
    }
}

// ---------------------------------------------------------------------------
// Bookmarks
// ---------------------------------------------------------------------------

/// The playback bookmark store: an LRU-capped map from key to position.
///
/// Capped at 100 like the other apps. The cap is not a technical limit; it is
/// what makes an old bookmark expire by attrition instead of by timestamp, and
/// it keeps a library listened to across years from growing this file without
/// bound.
pub const BOOKMARK_CAP: usize = 100;

pub struct BookmarkStore {
    entries: Arc<RwLock<Vec<(String, PlaybackBookmark)>>>,
    writer: SharedWriter,
    _thread: WriterThread,
    load_report: LoadReport,
    /// Trailing write coalescing: an immediate save is allowed, then further
    /// saves within [`BOOKMARK_MIN_INTERVAL`] are deferred so continuous
    /// playback writes at most once a second.
    last_write: Mutex<std::time::Instant>,
}

/// Minimum spacing between bookmark writes during playback.
pub const BOOKMARK_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

impl BookmarkStore {
    pub fn open() -> std::io::Result<Self> {
        let path = paths::bookmarks_file();
        let existed = path.exists();
        let (raw, outcome) = sn_core::atomic::load_json_recoverable::<
            std::collections::HashMap<String, PlaybackBookmark>,
        >(&path);
        let (writer, thread) = DebouncedWriter::new(path, BOOKMARK_MIN_INTERVAL)?.share();
        let mut entries: Vec<(String, PlaybackBookmark)> = raw.into_iter().collect();
        // Recency is rebuilt from `savedAt`, not from file order, so a
        // hand-edited or foreign file still evicts the oldest entry.
        entries.sort_by(|a, b| b.1.saved_at.raw().partial_cmp(&a.1.saved_at.raw()).unwrap());
        entries.truncate(BOOKMARK_CAP);
        Ok(Self {
            entries: Arc::new(RwLock::new(entries)),
            writer,
            _thread: thread,
            load_report: LoadReport::from_outcome(&outcome, existed),
            last_write: Mutex::new(std::time::Instant::now() - BOOKMARK_MIN_INTERVAL),
        })
    }

    pub fn load_report(&self) -> &LoadReport {
        &self.load_report
    }

    pub fn len(&self) -> usize {
        self.entries.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, key: &str) -> Option<PlaybackBookmark> {
        self.entries
            .read()
            .unwrap()
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, b)| b.clone())
    }

    /// The bookmark for a note, if it still describes the note's text.
    ///
    /// `None` on a hash mismatch is the behaviour the Swift store lacked: an
    /// edited note's stale bookmark *not* being offered for resume. A hash of
    /// zero (an iOS-written bookmark) is accepted so foreign files keep working.
    pub fn for_note(&self, note_id: &str, text: &str) -> Option<PlaybackBookmark> {
        let b = self.get(&note_key(note_id))?;
        if b.matches_text(text) {
            Some(b)
        } else {
            None
        }
    }

    pub fn for_book_chapter(&self, book_id: &str, chapter: usize) -> Option<PlaybackBookmark> {
        self.get(&book_key(book_id, chapter))
    }

    /// The newest note bookmark saved within `window`, for the resume banner.
    pub fn resume_candidate(&self, window: std::time::Duration) -> Option<(String, PlaybackBookmark)> {
        let cutoff = AppleDate::now().to_unix() - window.as_secs_f64();
        self.entries
            .read()
            .unwrap()
            .iter()
            .find(|(k, b)| k.starts_with("note:") && b.saved_at.to_unix() >= cutoff)
            .map(|(k, b)| (k.clone(), b.clone()))
    }

    /// Record a position. Writes are coalesced to one per second during
    /// playback; call [`Self::flush`] at the end of a run so the trailing write
    /// is not lost.
    pub fn put(&self, key: String, bookmark: PlaybackBookmark) {
        {
            let mut entries = self.entries.write().unwrap();
            entries.retain(|(k, _)| *k != key);
            entries.insert(0, (key, bookmark));
            entries.truncate(BOOKMARK_CAP);
        }
        self.persist();
    }

    /// Record and force a write, for stop/end-of-run.
    pub fn put_now(&self, key: String, bookmark: PlaybackBookmark) {
        self.put(key, bookmark);
        self.flush();
    }

    pub fn remove(&self, key: &str) {
        let mut entries = self.entries.write().unwrap();
        entries.retain(|(k, _)| *k != key);
        drop(entries);
        self.persist();
    }

    /// Remove every bookmark for a purged note.
    pub fn remove_note(&self, note_id: &str) {
        self.remove(&note_key(note_id));
    }

    /// The resume position for a note as a **UTF-8 byte offset** into `text`,
    /// or `None` when there is no bookmark or its hash no longer matches.
    ///
    /// This is the unit conversion the chunker's contract promises: the
    /// pipeline speaks in bytes, the file stores UTF-16 (the iOS/Swift
    /// contract), and this method is the one place the two meet. A bookmark
    /// written by iOS therefore resumes correctly here, and ours resumes
    /// correctly there.
    pub fn note_resume_byte(&self, note_id: &str, text: &str) -> Option<usize> {
        let b = self.for_note(note_id, text)?;
        Some(crate::model::utf16_to_byte_offset(text, b.text_offset))
    }

    /// Record a note's position from a UTF-8 byte frontier, hashing `text`
    /// for the validity check.
    ///
    /// Writes through to the file with the store's usual coalescing; the
    /// `_now` variant forces the flush for end-of-run.
    pub fn put_note_frontier(&self, note_id: &str, text: &str, byte_frontier: usize) {
        self.put_note_frontier_flushing(note_id, text, byte_frontier, false);
    }

    /// [`Self::put_note_frontier`] with a forced write, for stop/end-of-run.
    pub fn put_note_frontier_now(&self, note_id: &str, text: &str, byte_frontier: usize) {
        self.put_note_frontier_flushing(note_id, text, byte_frontier, true);
    }

    fn put_note_frontier_flushing(
        &self,
        note_id: &str,
        text: &str,
        byte_frontier: usize,
        now: bool,
    ) {
        let bookmark =
            PlaybackBookmark::for_note(note_id, crate::model::utf16_offset(text, byte_frontier), util::stable_hash(text));
        let key = note_key(note_id);
        if now {
            self.put_now(key, bookmark);
        } else {
            self.put(key, bookmark);
        }
    }

    fn persist(&self) {
        // A trailing guaranteed write: an immediate save is allowed, then the
        // next one inside the interval is deferred. The Swift port's re-arming
        // debounce meant continuous playback never wrote anything at all.
        let mut last = self.last_write.lock().unwrap();
        let elapsed = last.elapsed();
        if elapsed >= BOOKMARK_MIN_INTERVAL {
            *last = std::time::Instant::now();
            drop(last);
            self.write_now();
        } else {
            drop(last);
            let entries = self.entries.read().unwrap().clone();
            self.writer.schedule(self.encode(&entries));
        }
    }

    fn encode(&self, entries: &[(String, PlaybackBookmark)]) -> Vec<u8> {
        let map: std::collections::HashMap<&String, &PlaybackBookmark> =
            entries.iter().map(|(k, b)| (k, b)).collect();
        serde_json::to_vec_pretty(&map)
            .map(|mut b| {
                b.push(b'\n');
                b
            })
            .unwrap_or_default()
    }

    fn write_now(&self) {
        let entries = self.entries.read().unwrap().clone();
        let bytes = self.encode(&entries);
        self.writer.now(bytes);
    }

    /// Block until the current position is on disk. Call at stop and on quit.
    pub fn flush(&self) {
        self.write_now();
        let _ = self.writer.flush();
    }
}
