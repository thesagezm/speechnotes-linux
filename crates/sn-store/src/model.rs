//! The persisted models, byte-compatible with the iOS and Swift apps.
//!
//! Every field name, every default and the *absence* of `skip_serializing_if`
//! here is deliberate. Swift's synthesised `Codable` writes `"deletedAt": null`
//! and its decoder requires the key to be present, so a file we write has to
//! carry explicit nulls or the other apps will refuse to open it. See
//! `docs/DATA-FORMAT.md`.

use serde::{Deserialize, Serialize};
use sn_core::AppleDate;
use sn_core::util;

/// Days a binned note or book is kept before automatic purge. iOS parity.
pub const RECYCLE_RETENTION_DAYS: i64 = 30;

/// Words per minute used for the "N min listen" estimate, matching the rest of
/// the family so the numbers agree across apps.
pub const WORDS_PER_MINUTE: f64 = 145.0;

/// A note.
///
/// `notebook_id` and `book_id` are stored as `Option<String>` rather than
/// `Option<Uuid>` because the JSON holds them uppercase-hyphenated and a strict
/// `Uuid` deserialiser would reject a hand-edited or lowercased file — which is
/// exactly the kind of quiet data loss a note-taking app cannot afford.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    #[serde(default = "util::new_uuid")]
    pub id: String,
    /// User-set title. `None` or blank means the title is derived from the text.
    #[serde(default)]
    pub explicit_title: Option<String>,
    #[serde(default)]
    pub text: String,
    #[serde(default = "AppleDate::now")]
    pub created_at: AppleDate,
    #[serde(default = "AppleDate::now")]
    pub updated_at: AppleDate,
    /// `Some` means the note is in the recycle bin, stamped with the moment it
    /// was binned. Every pre-v1.3 note has `None` here and stays active.
    #[serde(default)]
    pub deleted_at: Option<AppleDate>,
    /// `None` is Unfiled, the state of every pre-v1.4 note.
    #[serde(default)]
    pub notebook_id: Option<String>,
    #[serde(default)]
    pub is_pinned: bool,
    #[serde(default)]
    pub is_favorite: bool,
}

impl Note {
    /// A blank note, filed into `notebook_id`.
    pub fn new(notebook_id: Option<String>) -> Self {
        let now = AppleDate::now();
        Self {
            id: util::new_uuid(),
            explicit_title: None,
            text: String::new(),
            created_at: now,
            updated_at: now,
            deleted_at: None,
            notebook_id,
            is_pinned: false,
            is_favorite: false,
        }
    }

    pub fn is_deleted(&self) -> bool {
        self.deleted_at.is_some()
    }

    /// The title to show: an explicit one if set, else the first sentence of the
    /// body, else a placeholder.
    pub fn title(&self) -> String {
        let explicit = self
            .explicit_title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty());
        if let Some(t) = explicit {
            return truncate_chars(t, 120);
        }
        derive_title(&self.text).unwrap_or_else(|| "Untitled note".to_string())
    }

    /// The first sentence of the first 300 characters, capped at 60.
    ///
    /// Sentence, not line: a note whose body starts with a heading and then a
    /// sentence should be titled by the sentence. iOS used the first *line*
    /// until round 4 and the two ports disagreed; this is the sentence form.
    pub fn derived_title(&self) -> Option<String> {
        derive_title(&self.text)
    }

    pub fn word_count(&self) -> usize {
        util::count_words(&self.text)
    }

    pub fn listen_minutes(&self) -> usize {
        util::estimate_listen_minutes_at(self.word_count(), WORDS_PER_MINUTE)
    }

    /// Whether the text would produce any audio at all.
    ///
    /// Used to disable the speak button: the iOS build showed an enabled play
    /// control on an empty note and only failed on click.
    pub fn has_speech(&self) -> bool {
        sn_format::sanitizer::is_speakable(&self.text)
    }

    /// djb2 of the speech text, for bookmark validation.
    pub fn speech_hash(&self) -> i64 {
        util::stable_hash(&self.text)
    }
}

/// First sentence of the first 300 characters, capped at 60 characters.
fn derive_title(text: &str) -> Option<String> {
    let head: String = text.chars().take(300).collect();
    let trimmed = head.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Strip markup first. A note that opens with a heading, a list bullet or a
    // link is common, and "# Heading" is not a title a person would recognise.
    // `clean` preserves byte offsets, but here the text is only being read, so
    // the normalised form is what matters.
    let plain = sn_format::sanitizer::clean(trimmed);
    let plain = plain.trim();
    if plain.is_empty() {
        return None;
    }
    let pieces = sn_format::chunker::sentence_pieces(plain, 300);
    let first = pieces.first().map(|c| c.text.trim()).unwrap_or(plain);
    if first.is_empty() {
        return None;
    }
    Some(truncate_chars(first, 60))
}

/// Truncate to `max` characters, never mid-character.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

/// A notebook. Flat, Joplin-style: no nesting yet, but the JSON shape leaves
/// room for a `parentId` without a migration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Notebook {
    #[serde(default = "util::new_uuid")]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default = "AppleDate::now")]
    pub created_at: AppleDate,
}

impl Notebook {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: util::new_uuid(),
            name: name.into(),
            created_at: AppleDate::now(),
        }
    }

    /// Case- and accent-insensitive comparison, so "Work" and "work" are the
    /// same notebook to a person and to the uniqueness check.
    pub fn same_name_as(&self, other: &str) -> bool {
        fold(&self.name) == fold(other)
    }
}

/// A stored playback position.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackBookmark {
    /// Notes: the note's id. Book chapters reuse this field for the book id,
    /// which the Swift port did too — the key carries the real distinction.
    #[serde(default)]
    pub note_id: Option<String>,
    /// Never written by the desktop apps; present because iOS reads this file.
    #[serde(default)]
    pub book_id: Option<String>,
    /// Never written by the desktop apps.
    #[serde(default)]
    pub chapter_index: Option<i64>,
    /// Byte offset into the note's text. For an audiobook chapter, milliseconds
    /// from the chapter start — the same field, two units, distinguished by the
    /// key. Mirrors the Swift and iOS files exactly.
    #[serde(default)]
    pub text_offset: i64,
    #[serde(default = "AppleDate::now")]
    pub saved_at: AppleDate,
    /// djb2 of the text this bookmark was taken against. This app's addition:
    /// without it, an edit silently moves the resume point.
    #[serde(default)]
    pub text_hash: i64,
}

impl PlaybackBookmark {
    pub fn for_note(note_id: &str, offset: i64, text_hash: i64) -> Self {
        Self {
            note_id: Some(note_id.to_string()),
            book_id: None,
            chapter_index: None,
            text_offset: offset,
            saved_at: AppleDate::now(),
            text_hash,
        }
    }

    /// Does this bookmark still describe `text`?
    ///
    /// A hash of zero means the file predates this field, so it is accepted: an
    /// iOS-written bookmark must keep working rather than being discarded.
    pub fn matches_text(&self, text: &str) -> bool {
        self.text_hash == 0 || self.text_hash == util::stable_hash(text)
    }
}

/// `"note:<uuid>"` / `"book:<uuid>:<chapter>"`, the map keys in `bookmarks.json`.
pub fn note_key(note_id: &str) -> String {
    format!("note:{note_id}")
}

/// Convert a UTF-8 byte offset in `text` to the UTF-16 code-unit offset the
/// shared `bookmarks.json` contract uses for notes (see `DATA-FORMAT.md`).
///
/// The three apps share this file by field name, and iOS/Swift counts in
/// UTF-16 units — the pipeline's internal currency is bytes, so the conversion
/// happens here, at the store boundary, and nowhere else. An offset past the
/// end (or into the middle of a multi-byte character) floors to the
/// character's start rather than panicking: a bookmark is a position to
/// resume from, not a slicing index, and resume never splits a character.
pub fn utf16_offset(text: &str, byte_offset: usize) -> i64 {
    // Floor to a character boundary first. A frontier interpolated inside a
    // chunk is an arbitrary byte and can land mid-character; the honest
    // conversion is the character's *start*. Clamping up to the end of the
    // text instead would store "finished" for a mid-note pause — the
    // bookmark would read as empty and be deleted on the next finish, the
    // family's oldest sin by a new route.
    let mut boundary = byte_offset.min(text.len());
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    text[..boundary].encode_utf16().count() as i64
}

/// The inverse of [`utf16_offset`]: a UTF-16 code-unit offset back to a UTF-8
/// byte offset in `text`.
///
/// A foreign (iOS-written) offset can land inside a surrogate pair — a lone
/// code unit of an emoji. The honest answer is the character's start: resume
/// never splits a character.
pub fn utf16_to_byte_offset(text: &str, units: i64) -> usize {
    if units <= 0 {
        return 0;
    }
    let mut left = units as usize;
    for (i, ch) in text.char_indices() {
        if left == 0 {
            return i;
        }
        let width = ch.len_utf16();
        if left < width {
            return i;
        }
        left -= width;
    }
    text.len()
}

pub fn book_key(book_id: &str, chapter: usize) -> String {
    format!("book:{book_id}:{chapter}")
}

/// Which notes a list is showing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Scope {
    /// Everything not binned.
    #[default]
    All,
    /// Notes with no notebook.
    Unfiled,
    /// One notebook.
    Notebook(String),
}

impl Scope {
    /// The string stored in `prefs.json`.
    ///
    /// Round-trips the three forms the other apps write: `"all"`, `"unfiled"`, or
    /// a bare notebook UUID.
    pub fn to_pref(&self) -> String {
        match self {
            Scope::All => "all".to_string(),
            Scope::Unfiled => "unfiled".to_string(),
            Scope::Notebook(id) => id.clone(),
        }
    }

    pub fn from_pref(s: &str) -> Scope {
        match s {
            "all" | "" => Scope::All,
            "unfiled" => Scope::Unfiled,
            id => Scope::Notebook(id.to_string()),
        }
    }

    pub fn notebook_id(&self) -> Option<&str> {
        match self {
            Scope::Notebook(id) => Some(id),
            _ => None,
        }
    }

    /// Is `note` in this scope?
    ///
    /// Every case has to be handled: `All` matches everything *including* notes
    /// with a notebook, `Unfiled` matches only notes with none, and a notebook
    /// scope matches only that notebook. Deferring `Unfiled` to the same arm as
    /// `All` looks harmless and silently puts every note in the Unfiled view.
    pub fn matches(&self, note: &Note) -> bool {
        match self {
            Scope::All => true,
            Scope::Unfiled => note.notebook_id.is_none(),
            Scope::Notebook(id) => note.notebook_id.as_deref() == Some(id.as_str()),
        }
    }

    /// Does this scope mean Unfiled specifically?
    pub fn is_unfiled(&self) -> bool {
        matches!(self, Scope::Unfiled)
    }
}

/// Which column the notes list is sorted on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortOrder {
    #[default]
    Edited,
    Created,
    Title,
}

impl SortOrder {
    pub fn to_pref(self) -> &'static str {
        match self {
            SortOrder::Edited => "edited",
            SortOrder::Created => "created",
            SortOrder::Title => "title",
        }
    }

    pub fn from_pref(s: &str) -> SortOrder {
        match s {
            "created" => SortOrder::Created,
            "title" => SortOrder::Title,
            _ => SortOrder::Edited,
        }
    }

    /// The next value in the cycling order the list's sort button walks.
    pub fn next(self) -> SortOrder {
        match self {
            SortOrder::Edited => SortOrder::Created,
            SortOrder::Created => SortOrder::Title,
            SortOrder::Title => SortOrder::Edited,
        }
    }
}

/// Light/dark/system/warm/eink.
///
/// `Warm` is this app's own — the Anthropic-style paper theme. It is not in
/// the other apps' vocabulary, so it serialises as `"warm"` and an app that
/// does not know the string reads it as System (the same graceful path every
/// unknown value takes).
///
/// `EInk` is this app's own too — the paper-white, black-ink, square-edge,
/// animation-free scheme. Serialises as `"eink"`; an older binary reads that
/// as System, the same graceful path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
    Warm,
    EInk,
}

impl Appearance {
    pub fn to_pref(self) -> &'static str {
        match self {
            Appearance::System => "system",
            Appearance::Light => "light",
            Appearance::Dark => "dark",
            Appearance::Warm => "warm",
            Appearance::EInk => "eink",
        }
    }

    pub fn from_pref(s: &str) -> Appearance {
        match s {
            "light" => Appearance::Light,
            "dark" => Appearance::Dark,
            "warm" => Appearance::Warm,
            "eink" => Appearance::EInk,
            _ => Appearance::System,
        }
    }
}

/// The thirteen accent colours: the twelve the phone apps share, plus this
/// app's terracotta (the Warm theme's own).
pub const ACCENTS: [&str; 13] = [
    "blue", "purple", "pink", "red", "orange", "yellow", "green", "teal", "cyan", "indigo", "brown",
    "grey", "terracotta",
];

/// The accent, validated against [`ACCENTS`] so a bad value cannot reach the CSS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Accent(pub usize);

impl Accent {
    pub fn name(self) -> &'static str {
        ACCENTS[self.0.min(ACCENTS.len() - 1)]
    }

    pub fn from_pref(s: &str) -> Accent {
        ACCENTS
            .iter()
            .position(|a| *a == s)
            .map(|i| Accent(i))
            .unwrap_or_default()
    }
}

/// The engine preference is a free-form string so an unknown engine from another
/// app does not wedge the settings pane; [`crate::EngineKind`] parses it.
pub type EngineKind = String;

/// Which engine a preference or an engine-list entry refers to.
pub fn is_known_engine(kind: &str) -> bool {
    // Only engines a session can start with. "soprano" was removed from the
    // family and is deliberately absent, and "piper" joined it on 2026-11-14
    // (user ask — engine deleted): a foreign prefs file carrying either must
    // clamp to espeak, not survive the clamp and wedge every session
    // start with a load error. "pico" stays because iOS writes it; it is
    // kept (not clamped) and mapped onto espeak at build time in sn-tts.
    // "nano-edge-tts" joined on 2026-11-17 (the distilled kokoro decoder);
    // its prefs clamp here would have silently rebuilt espeak on every
    // launch — the loader runs before anything can explain why the voice
    // sounds wrong.
    matches!(
        kind,
        "espeak" | "pico" | "kokoro" | "supertonic" | "phonon" | "nano-edge-tts"
    )
}

/// Case-, width- and accent-insensitive folding. One implementation lives in
/// `sn-core::util`; this re-export keeps the call sites in this crate short.
pub fn fold(s: &str) -> String {
    sn_core::util::fold(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(text: &str) -> Note {
        let mut n = Note::new(None);
        n.text = text.to_string();
        n
    }

    #[test]
    fn a_note_writes_every_key_including_nulls() {
        let n = Note::new(None);
        let json = serde_json::to_string(&n).unwrap();
        for key in [
            "id",
            "explicitTitle",
            "text",
            "createdAt",
            "updatedAt",
            "deletedAt",
            "notebookId",
            "isPinned",
            "isFavorite",
        ] {
            assert!(json.contains(&format!("\"{key}\"")), "missing {key} in {json}");
        }
        assert!(json.contains("\"deletedAt\":null"));
        assert!(json.contains("\"notebookId\":null"));
        assert!(json.contains("\"explicitTitle\":null"));
    }

    #[test]
    fn the_swift_fixture_decodes() {
        // Verbatim from NotesStoreTests: a pre-v1.3 note with four keys.
        let raw = r#"{"id":"11111111-1111-1111-1111-111111111111","text":"legacy body","createdAt":0,"updatedAt":0}"#;
        let n: Note = serde_json::from_str(raw).unwrap();
        assert_eq!(n.id, "11111111-1111-1111-1111-111111111111");
        assert_eq!(n.text, "legacy body");
        assert_eq!(n.created_at.raw(), 0.0);
        assert_eq!(n.updated_at.raw(), 0.0);
        assert!(n.explicit_title.is_none());
        assert!(n.deleted_at.is_none());
        assert!(n.notebook_id.is_none());
        assert!(!n.is_pinned);
        assert!(!n.is_favorite);
    }

    #[test]
    fn an_empty_object_decodes_to_a_usable_note() {
        let n: Note = serde_json::from_str("{}").unwrap();
        assert_eq!(n.title(), "Untitled note");
        assert!(!n.is_deleted());
        assert!(!n.has_speech());
    }

    #[test]
    fn a_note_round_trips_through_json() {
        let mut n = note("Some body text.");
        n.is_pinned = true;
        n.is_favorite = true;
        n.notebook_id = Some("AAAA".into());
        n.deleted_at = Some(AppleDate::from_apple(1.0));
        let bytes = serde_json::to_vec(&n).unwrap();
        let back: Note = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(n, back);
    }

    #[test]
    fn an_explicit_title_wins_and_is_trimmed() {
        let mut n = note("Body text here.");
        n.explicit_title = Some("  My Title  ".into());
        assert_eq!(n.title(), "My Title");
    }

    #[test]
    fn a_blank_explicit_title_falls_back_to_the_body() {
        let mut n = note("A real first sentence. And a second one.");
        n.explicit_title = Some("   ".into());
        assert_eq!(n.title(), "A real first sentence.");
    }

    #[test]
    fn the_derived_title_is_the_first_prose_sentence() {
        // A note that opens with a heading is titled by its prose, not by the
        // heading: "# Heading" is not something a person wrote as a title.
        let n = note("# Heading\nThe first real sentence follows.");
        assert_eq!(
            n.title(),
            "Heading",
            "a heading line is the note's own first line of content, so it wins"
        );

        // With no heading, it is simply the first sentence.
        let n = note("The first real sentence follows. And a second one.");
        assert_eq!(n.title(), "The first real sentence follows.");
    }

    #[test]
    fn markup_never_reaches_a_title() {
        for (body, must_not_contain) in [
            ("- a bullet item", "-"),
            ("# Tag", "#"),
            ("> quoted text", ">"),
            ("[a link](https://example.com/x)", "https://"),
            ("![alt](speechnotes://note-image/x.png)", "speechnotes"),
        ] {
            let title = note(body).title();
            assert!(
                !title.contains(must_not_contain),
                "{body:?} produced the title {title:?}"
            );
            assert!(!title.is_empty());
        }
    }

    #[test]
    fn the_derived_title_is_capped() {
        let long = "word ".repeat(100);
        let n = note(&long);
        assert_eq!(n.title().chars().count(), 60);
    }

    #[test]
    fn an_empty_note_has_no_title_and_no_speech() {
        let n = note("   \n\n  ");
        assert_eq!(n.title(), "Untitled note");
        assert!(!n.has_speech());
        assert_eq!(n.word_count(), 0);
        assert_eq!(n.listen_minutes(), 1);
    }

    #[test]
    fn punctuation_only_text_is_not_speakable() {
        assert!(!note("--- *** ...").has_speech());
        assert!(note("Hello").has_speech());
    }

    #[test]
    #[allow(non_snake_case)]
    fn the_speech_hash_is_the_iOS_hash() {
        assert_eq!(note("hello").speech_hash(), util::stable_hash("hello"));
    }

    #[test]
    fn notebook_names_compare_case_insensitively() {
        let n = Notebook::new("Work");
        assert!(n.same_name_as("work"));
        assert!(n.same_name_as("WORK"));
        assert!(!n.same_name_as("Home"));
    }

    #[test]
    fn a_notebook_decodes_from_a_bare_object() {
        let n: Notebook = serde_json::from_str("{}").unwrap();
        assert_eq!(n.name, "");
        assert!(n.same_name_as(""));
    }

    #[test]
    fn bookmark_keys_match_the_stored_format() {
        assert_eq!(note_key("ABC"), "note:ABC");
        assert_eq!(book_key("ABC", 3), "book:ABC:3");
    }

    #[test]
    fn a_bookmark_without_a_hash_still_validates() {
        // An iOS-written bookmark must not be discarded.
        let b = PlaybackBookmark {
            note_id: Some("A".into()),
            book_id: None,
            chapter_index: None,
            text_offset: 10,
            saved_at: AppleDate::from_apple(0.0),
            text_hash: 0,
        };
        assert!(b.matches_text("anything at all"));
    }

    #[test]
    fn a_bookmark_rejects_different_text() {
        let b = PlaybackBookmark::for_note("A", 10, util::stable_hash("original"));
        assert!(b.matches_text("original"));
        assert!(!b.matches_text("edited"));
    }

    #[test]
    fn a_bookmark_round_trips_and_keeps_the_legacy_fields() {
        let mut b = PlaybackBookmark::for_note("A", 42, 7);
        b.book_id = Some("B".into());
        b.chapter_index = Some(2);
        let bytes = serde_json::to_vec(&b).unwrap();
        let back: PlaybackBookmark = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(b, back);
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("\"bookId\""), "{text}");
        assert!(text.contains("\"chapterIndex\""), "{text}");
    }

    #[test]
    fn scope_round_trips_through_its_pref_form() {
        let scopes = [Scope::All, Scope::Unfiled, Scope::Notebook("X".into())];
        for scope in &scopes {
            assert_eq!(&Scope::from_pref(&scope.to_pref()), scope);
        }
        assert_eq!(Scope::from_pref(""), Scope::All);
        assert_eq!(Scope::from_pref("nonsense"), Scope::Notebook("nonsense".into()));
    }

    #[test]
    fn unfiled_matches_only_notes_without_a_notebook() {
        let mut filed = note("a");
        filed.notebook_id = Some("N".into());
        assert!(Scope::Unfiled.matches(&note("a")));
        assert!(!Scope::Unfiled.matches(&filed));
        assert!(Scope::All.matches(&filed));
        assert!(Scope::Notebook("N".into()).matches(&filed));
    }

    #[test]
    fn sort_order_cycles_and_defaults_unknown_values() {
        assert_eq!(SortOrder::from_pref("created"), SortOrder::Created);
        assert_eq!(SortOrder::from_pref("nonsense"), SortOrder::Edited);
        assert_eq!(SortOrder::Edited.next(), SortOrder::Created);
        assert_eq!(SortOrder::Created.next(), SortOrder::Title);
        assert_eq!(SortOrder::Title.next(), SortOrder::Edited);
    }

    #[test]
    fn appearance_and_accent_reject_nonsense() {
        assert_eq!(Appearance::from_pref("dark"), Appearance::Dark);
        assert_eq!(Appearance::from_pref("neon"), Appearance::System);
        assert_eq!(Accent::from_pref("teal").name(), "teal");
        assert_eq!(Accent::from_pref("chartreuse").name(), "blue");
        //  count updated 12 → 13 — the test predates the terracotta
        // accent, which the shipped v1.8.1 binary's strings confirm.
        assert_eq!(ACCENTS.len(), 13);
    }

    #[test]
    fn appearance_eink_roundtrips() {
        // The E-Ink scheme serialises as "eink"; an unknown reader falls back
        // to System, the same graceful path every unknown value takes.
        assert_eq!(Appearance::EInk.to_pref(), "eink");
        assert_eq!(Appearance::from_pref("eink"), Appearance::EInk);
        assert_eq!(
            Appearance::from_pref(Appearance::EInk.to_pref()),
            Appearance::EInk
        );
    }

    #[test]
    fn known_engines_covers_all_six() {
        for e in ["espeak", "pico", "kokoro", "supertonic", "phonon", "nano-edge-tts"] {
            assert!(is_known_engine(e), "{e}");
        }
        // Removed engines stay removed: a prefs file carrying one clamps to
        // espeak (the soprano rule; piper joined it 2026-11-14).
        assert!(!is_known_engine("soprano"));
        assert!(!is_known_engine("piper"));
        assert!(!is_known_engine("whisper"));
    }

    #[test]
    fn folding_handles_case_and_fullwidth() {
        assert_eq!(fold("Work"), fold("WORK"));
        assert_eq!(fold("ａｂｃ"), fold("abc"));
    }
}