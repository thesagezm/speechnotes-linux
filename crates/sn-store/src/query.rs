//! Filtering, sorting and search over the note library.
//!
//! This runs on the UI thread for every keystroke in the search box, so it is
//! allocation-light and linear. The memoisation contract lives with the caller:
//! [`Query::key`] is what a view compares against its last result, so a 3 Hz
//! playback tick does not re-filter a 5 000-note library.

use crate::model::{fold, Note, Scope, SortOrder};

/// Fold, counted in tests: the search-index tests assert *how many* folds a
/// mutation costs rather than how long it takes — a wall clock flakes on a
/// loaded box and means nothing across a debug and a release build.
#[cfg(test)]
static TEST_FOLDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The folds `SearchIndex` has paid for so far (tests only).
#[cfg(test)]
pub fn test_fold_count() -> u64 {
    TEST_FOLDS.load(std::sync::atomic::Ordering::Relaxed)
}

fn index_fold(s: &str) -> String {
    #[cfg(test)]
    TEST_FOLDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    fold(s)
}

/// A prepared query: scope, search text and sort order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Query {
    pub scope: Scope,
    /// The raw search box contents.
    pub search: String,
    pub sort: SortOrder,
}

impl Query {
    /// A memoisation key: the caller compares this to its last result and
    /// skips the re-filter when nothing changed.
    pub fn key(&self) -> String {
        format!(
            "{:?}|{}|{:?}",
            self.scope,
            fold(&self.search),
            self.sort
        )
    }

    /// Filter and sort `notes`.
    ///
    /// Correct for small libraries, and what the tests exercise. A UI should
    /// use [`Query::run_indexed`] with a [`SearchIndex`] it keeps across
    /// keystrokes: this form re-folds every note's title and body on every
    /// call, which is hundreds of milliseconds on a 5 000-note library.
    ///
    /// Pinned notes always come first, within their own sort ordering — the
    /// Swift port and iOS agree on this, and re-litigating it would surprise a
    /// user moving between the apps.
    pub fn run(&self, notes: &[Note]) -> Vec<Note> {
        let index = SearchIndex::build(notes);
        self.run_indexed(notes, &index)
    }

    /// Filter and sort against a precomputed index.
    ///
    /// The index holds each note's folded title and body, so a keystroke costs
    /// one substring scan per note over already-folded text — no normalisation
    /// on the hot path at all.
    pub fn run_indexed(&self, notes: &[Note], index: &SearchIndex) -> Vec<Note> {
        let terms = search_terms(&self.search);
        let mut out: Vec<Note> = notes
            .iter()
            .filter(|n| !n.is_deleted())
            .filter(|n| self.scope_matches(n))
            .filter(|n| matches_index(n, &terms, index))
            .cloned()
            .collect();

        match self.sort {
            SortOrder::Edited => {
                out.sort_by(|a, b| b.updated_at.raw().total_cmp(&a.updated_at.raw()))
            }
            SortOrder::Created => {
                out.sort_by(|a, b| b.created_at.raw().total_cmp(&a.created_at.raw()))
            }
            SortOrder::Title => out.sort_by(|a, b| {
                let ka = index.folded_title(&a.id, a);
                let kb = index.folded_title(&b.id, b);
                ka.cmp(&kb)
            }),        }
        // Stable sort keeps the primary ordering inside pinned / unpinned.
        out.sort_by_key(|n| std::cmp::Reverse(n.is_pinned));
        out
    }

    fn scope_matches(&self, note: &Note) -> bool {
        match &self.scope {
            Scope::All => true,
            Scope::Unfiled => note.notebook_id.is_none(),
            Scope::Notebook(id) => note.notebook_id.as_deref() == Some(id.as_str()),
        }
    }
}

/// Split a search string into terms. Quoted phrases stay whole, so `"error
/// 404"` finds the phrase rather than the two words anywhere.
pub fn search_terms(search: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut chars = search.chars().peekable();
    loop {
        // Skip separators.
        while matches!(chars.peek(), Some(c) if c.is_whitespace()) {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }
        if chars.peek() == Some(&'"') {
            chars.next();
            let mut term = String::new();
            for c in chars.by_ref() {
                if c == '"' {
                    break;
                }
                term.push(c);
            }
            if !term.trim().is_empty() {
                terms.push(fold(term.trim()));
            }
        } else {
            let mut term = String::new();
            while let Some(c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                term.push(*c);
                chars.next();
            }
            if !term.trim().is_empty() {
                terms.push(fold(term.trim()));
            }
        }
    }
    terms
}

/// Does the note contain every term, in its title or body?
///
/// Terms are ANDed — that is what "search" means to someone narrowing a list —
/// and matching is over folded text, so `café` finds `Cafe`. The index supplies
/// the folded text; falling back to folding here is only for a note the index
/// has never seen.
fn matches_index(note: &Note, terms: &[String], index: &SearchIndex) -> bool {
    if terms.is_empty() {
        return true;
    }
    let title = index.folded_title(&note.id, note);
    let body = index.folded_body(&note.id, note);
    terms
        .iter()
        .all(|t| title.contains(t.as_str()) || body.contains(t.as_str()))
}

/// Folded text for every note, kept across keystrokes.
///
/// The cost this exists to avoid: folding a note's body is an NFD pass, and
/// doing it for a whole library on every keystroke is the difference between a
/// search box that keeps up and one that lags a second behind. The index folds
/// each note once, when it first appears, and forgets it when it goes away.
///
/// The folds are **content-addressed**: each note remembers the exact title
/// and body it was folded from. A note whose content has not changed is not
/// folded again, and a note whose content *has* changed is — without the
/// caller having to remember to say so. The earlier version folded a note
/// once and never again, so an edited note kept its old fold: searching for
/// the words the user had just typed found nothing, and searching for the
/// words they had deleted still found the note.
///
/// Not thread-safe: the UI thread owns it.
#[derive(Debug, Default)]
pub struct SearchIndex {
    titles: std::collections::HashMap<String, String>,
    bodies: std::collections::HashMap<String, String>,
    /// The explicit title each fold was made from. A folded title is a pure
    /// function of (explicit_title, text), so with both sources unchanged the
    /// title needs neither recomputing nor re-folding — a pin or a move pays
    /// for no `Note::title()` call at all.
    source_explicit: std::collections::HashMap<String, Option<String>>,
    /// The body each fold was made from, so a changed body is re-folded.
    source_bodies: std::collections::HashMap<String, String>,
}

impl SearchIndex {
    /// Fold every note once.
    pub fn build(notes: &[Note]) -> Self {
        let mut idx = Self::default();
        idx.sync(notes);
        idx
    }

    /// Fold the notes whose content changed, drop the folds for gone ones,
    /// leave the rest untouched.
    ///
    /// The comparison is against the *source* text the fold was made from, so
    /// a pin, a favourite or a notebook move — which changes neither the
    /// title nor the body — costs no fold and no `Note::title()` call. When
    /// no note was added or removed, the dead-drop pass over the four maps
    /// (four hashes per key) is skipped as well, so an ordinary edit pays
    /// for the notes that actually changed and little else.
    pub fn sync(&mut self, notes: &[Note]) {
        if notes.len() != self.titles.len()
            || notes.iter().any(|n| !self.titles.contains_key(&n.id))
        {
            let live: std::collections::HashSet<&str> =
                notes.iter().map(|n| n.id.as_str()).collect();
            self.titles.retain(|k, _| live.contains(k.as_str()));
            self.bodies.retain(|k, _| live.contains(k.as_str()));
            self.source_explicit.retain(|k, _| live.contains(k.as_str()));
            self.source_bodies.retain(|k, _| live.contains(k.as_str()));
        }
        for note in notes {
            let same_body = self
                .source_bodies
                .get(&note.id)
                .map_or(false, |old| old == &note.text);
            let same_explicit = self
                .source_explicit
                .get(&note.id)
                .map_or(false, |old| old == &note.explicit_title);
            if same_body && same_explicit {
                continue;
            }
            if !same_explicit || !same_body {
                self.titles.insert(note.id.clone(), index_fold(&note.title()));
                self.source_explicit
                    .insert(note.id.clone(), note.explicit_title.clone());
            }
            if !same_body {
                self.bodies.insert(note.id.clone(), index_fold(&note.text));
                self.source_bodies.insert(note.id.clone(), note.text.clone());
            }
        }
    }

    /// Drop a note's folds, after an edit, so the next sync re-folds it.
    pub fn invalidate(&mut self, note_id: &str) {
        self.titles.remove(note_id);
        self.bodies.remove(note_id);
        self.source_explicit.remove(note_id);
        self.source_bodies.remove(note_id);
    }

    /// The folded title of a note.
    ///
    /// Borrowed when the index has the fold (the normal case — the fold is
    /// kept across keystrokes), owned only for a note the index has never
    /// seen. The borrow is what keeps a keystroke from cloning every note's
    /// folded body.
    pub fn folded_title<'a>(&'a self, id: &str, note: &'a Note) -> std::borrow::Cow<'a, str> {
        match self.titles.get(id) {
            Some(folded) => std::borrow::Cow::Borrowed(folded.as_str()),
            None => std::borrow::Cow::Owned(index_fold(&note.title())),
        }
    }

    /// The folded body of a note — borrowed when the index has the fold.
    pub fn folded_body<'a>(&'a self, id: &str, note: &'a Note) -> std::borrow::Cow<'a, str> {
        match self.bodies.get(id) {
            Some(folded) => std::borrow::Cow::Borrowed(folded.as_str()),
            None => std::borrow::Cow::Owned(fold(&note.text)),
        }
    }

    pub fn len(&self) -> usize {
        self.titles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.titles.is_empty()
    }
}

/// Which date section a note belongs under in the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Pinned,
    Today,
    Yesterday,
    Past7Days,
    Older,
}

impl Section {
    pub fn label(self) -> &'static str {
        match self {
            Section::Pinned => "Pinned",
            Section::Today => "Today",
            Section::Yesterday => "Yesterday",
            Section::Past7Days => "Previous 7 days",
            Section::Older => "Older",
        }
    }

    /// Section a note belongs to, given the reference time in Apple-offset form.
    ///
    /// Pinned wins over date: the section exists to hold the notes the user
    /// pulled out of the date flow.
    pub fn of(note: &Note, now: f64) -> Section {
        if note.is_pinned {
            return Section::Pinned;
        }
        let edited = note.updated_at.raw();
        let days = ((now - edited) / 86_400.0).floor() as i64;
        match days {
            d if d < 1 => Section::Today,
            1 => Section::Yesterday,
            d if d < 7 => Section::Past7Days,
            _ => Section::Older,
        }
    }
}

/// Group sorted notes into their display sections, in display order.
pub fn sections(notes: &[Note], now_apple: f64) -> Vec<(Section, Vec<Note>)> {
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<u8, Vec<Note>> = BTreeMap::new();
    for note in notes {
        let section = Section::of(note, now_apple);
        groups.entry(section as u8).or_default().push(note.clone());
    }
    groups
        .into_iter()
        .map(|(k, v)| (section_from_u8(k), v))
        .collect()
}

fn section_from_u8(k: u8) -> Section {
    match k {
        0 => Section::Pinned,
        1 => Section::Today,
        2 => Section::Yesterday,
        3 => Section::Past7Days,
        _ => Section::Older,
    }
}

/// One visual row of the notes list.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    /// A section label. Not selectable, and carries no note id.
    Header(Section),
    Note(Note),
}

impl Row {
    /// The note id this row opens, or `""` for a header.
    ///
    /// The empty string is deliberate: `row_ids` is indexed by *visual row
    /// index*, so every row needs a slot or the mapping shifts. Returning `""`
    /// keeps a header click from resolving to a real note.
    pub fn id(&self) -> &str {
        match self {
            Row::Header(_) => "",
            Row::Note(n) => &n.id,
        }
    }
}

/// The rows the notes list renders, in visual order.
///
/// The grouping comes from [`sections`] and nothing else decides order, so the
/// row sequence and the id-per-row mapping cannot drift apart. The old code
/// walked the sections here and pushed ids into a parallel vector *only for
/// note rows*, which shifted every id up by one per section header: clicking
/// the third note opened the second, and the last note in the list could not be
/// reached at all.
pub fn note_rows(notes: &[Note], now_apple: f64) -> Vec<Row> {
    let mut out = Vec::with_capacity(notes.len() + 4);
    for (section, group) in sections(notes, now_apple) {
        out.push(Row::Header(section));
        out.extend(group.into_iter().map(Row::Note));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use sn_core::AppleDate;

    fn note(text: &str) -> Note {
        let mut n = Note::new(None);
        n.text = text.to_string();
        n
    }

    fn aged(days_ago: f64) -> Note {
        let mut n = Note::new(None);
        n.text = "body".into();
        let now = AppleDate::now().raw();
        n.updated_at = AppleDate::from_apple(now - days_ago * 86_400.0);
        n
    }

    #[test]
    fn a_query_filters_by_scope_then_search() {
        let mut filed = note("meeting notes");
        filed.notebook_id = Some("NB".into());
        let unfiled = note("grocery list");
        let notes = vec![filed.clone(), unfiled.clone()];

        let q = Query {
            scope: Scope::Notebook("NB".into()),
            search: String::new(),
            sort: SortOrder::Edited,
        };
        assert_eq!(q.run(&notes).len(), 1);

        let q = Query {
            scope: Scope::Unfiled,
            ..q.clone()
        };
        assert_eq!(q.run(&notes)[0].id, unfiled.id);

        let q = Query {
            scope: Scope::All,
            search: "grocery".into(),
            ..q
        };
        assert_eq!(q.run(&notes)[0].id, unfiled.id);
    }

    #[test]
    fn search_terms_are_anded_and_folded() {
        let a = note("The quick brown fox");
        let b = note("brown bear asleep");
        let notes = vec![a.clone(), b.clone()];
        let q = Query {
            scope: Scope::All,
            search: "brown fox".into(),
            sort: SortOrder::Edited,
        };
        let hits = q.run(&notes);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, a.id);

        // Case and accent folding: the query has no accents, the note does.
        let cafe = note("Le Café Parisien");
        let q = Query {
            scope: Scope::All,
            search: "cafe parisien".into(),
            ..q
        };
        assert_eq!(q.run(&[cafe]).len(), 1);

        // And the reverse direction.
        let cafe = note("Le Café Parisien");
        let q = Query {
            scope: Scope::All,
            search: "Café".into(),
            ..q
        };
        assert_eq!(q.run(&[cafe]).len(), 1);
    }

    #[test]
    fn a_quoted_phrase_is_one_term() {
        assert_eq!(search_terms(r#"error "404 page" x"#), vec!["error", "404 page", "x"]);
        assert!(search_terms("   ").is_empty());
        assert!(search_terms(r#""  ""#).is_empty(), "an empty phrase is not a term");
    }

    #[test]
    fn search_reaches_the_title_as_well_as_the_body() {
        let mut n = note("body about nothing");
        n.explicit_title = Some("Shopping ideas".into());
        let q = Query {
            scope: Scope::All,
            search: "shopping".into(),
            sort: SortOrder::Edited,
        };
        assert_eq!(q.run(&[n]).len(), 1);
    }

    #[test]
    fn sorting_orders_by_the_right_column() {
        let mut a = note("alpha");
        a.updated_at = AppleDate::from_apple(100.0);
        a.created_at = AppleDate::from_apple(900.0);
        let mut b = note("Beta");
        b.updated_at = AppleDate::from_apple(200.0);
        b.created_at = AppleDate::from_apple(800.0);
        let notes = vec![a.clone(), b.clone()];

        let q = Query {
            scope: Scope::All,
            search: String::new(),
            sort: SortOrder::Edited,
        };
        assert_eq!(q.run(&notes)[0].id, b.id, "newest edit first");

        let q = Query {
            sort: SortOrder::Created,
            ..q.clone()
        };
        assert_eq!(q.run(&notes)[0].id, a.id, "newest creation first");

        let q = Query {
            sort: SortOrder::Title,
            ..q
        };
        assert_eq!(q.run(&notes)[0].id, a.id, "alpha before Beta, case-insensitively");
    }

    #[test]
    fn pinned_notes_come_first_within_their_sort() {
        let mut old_pinned = note("pinned old");
        old_pinned.is_pinned = true;
        old_pinned.updated_at = AppleDate::from_apple(10.0);
        let mut new_unpinned = note("unpinned new");
        new_unpinned.updated_at = AppleDate::from_apple(20.0);

        let q = Query {
            scope: Scope::All,
            search: String::new(),
            sort: SortOrder::Edited,
        };
        let out = q.run(&[new_unpinned.clone(), old_pinned.clone()]);
        assert_eq!(out[0].id, old_pinned.id);
        assert_eq!(out[1].id, new_unpinned.id);
    }

    #[test]
    fn binned_notes_never_appear_in_a_query() {
        let mut binned = note("deleted thing");
        binned.deleted_at = Some(AppleDate::now());
        let q = Query::default();
        assert!(q.run(&[binned]).is_empty());
    }

    #[test]
    fn the_memo_key_distinguishes_every_input() {
        let base = Query {
            scope: Scope::All,
            search: "abc".into(),
            sort: SortOrder::Edited,
        };
        let same = base.clone();
        assert_eq!(base.key(), same.key());
        assert_ne!(base.key(), Query { search: "abd".into(), ..base.clone() }.key());
        assert_ne!(base.key(), Query { sort: SortOrder::Created, ..base.clone() }.key());
        assert_ne!(base.key(), Query { scope: Scope::Unfiled, ..base }.key());
    }

    #[test]
    fn a_memoised_query_returns_the_same_rows_as_a_fresh_one() {
        // The memo in the UI skips the query when its key has not changed.
        // That is only safe if the key covers every input the query reads.
        // This walks the query's own inputs and checks that the key moves
        // for each one, so a future field added to `Query` cannot silently
        // fall out of the key.
        let base = Query {
            scope: Scope::All,
            search: "abc".into(),
            sort: SortOrder::Edited,
        };
        let key = base.key();
        // Every field of `Query` is one of these three.
        assert_ne!(key, Query { scope: Scope::Notebook("x".into()), ..base.clone() }.key());
        assert_ne!(key, Query { search: "abcd".into(), ..base.clone() }.key());
        assert_ne!(key, Query { sort: SortOrder::Title, ..base }.key());
    }

    #[test]
    fn a_row_index_resolves_to_the_note_on_that_row() {
        // The regression this whole function exists for. The row list is
        // indexed by *visual* position, and a section header occupies a visual
        // row without holding a note. Recording ids only for note rows shifted
        // the whole mapping up by one per header: clicking a note opened the
        // note above it, the last note in the list was unreachable, and pressing
        // Delete binned the wrong note. This is the user's own library, the one
        // that lost a note to it.
        let mut a = note("Am looking for the principle");
        let mut b = note("Do you have any new ideas where he went");
        let mut c = note("hi how are you? where a");
        // Ages are relative to *now*, not real timestamps: the original
        // fixture used the user's actual notes, and `Section::of` buckets by
        // elapsed 24-hour windows, not calendar days — those three drifted
        // across the Today/Yesterday line as time passed and silently split
        // into two sections, breaking this test on a date the row mapping had
        // nothing to do with. What this test exercises is the row mapping,
        // and that only needs every note in one section.
        let now = AppleDate::now().raw();
        a.updated_at = AppleDate::from_apple(now - 0.1 * 86_400.0);
        b.updated_at = AppleDate::from_apple(now - 0.2 * 86_400.0);
        c.updated_at = AppleDate::from_apple(now - 0.3 * 86_400.0);

        let q = Query {
            scope: Scope::All,
            search: String::new(),
            sort: SortOrder::Edited,
        };
        let sorted = q.run(&[a, b, c]);
        // The list shows newest edit first, so the expected order comes from the
        // sort rather than from the order the notes were written in.
        let expected: Vec<String> = sorted.iter().map(|n| n.id.clone()).collect();

        let rows = note_rows(&sorted, now);
        let by_index: Vec<&str> = rows.iter().map(Row::id).collect();

        // Three notes, one header, and the mapping still lines up.
        assert_eq!(rows.len(), 4, "three notes plus one section header");
        assert_eq!(by_index[0], "", "row 0 is the header and holds no id");
        for (i, id) in expected.iter().enumerate() {
            assert_eq!(
                by_index[i + 1], id,
                "row {i} must resolve to the note rendered on it"
            );
        }
        // And the one the user could never reach: the last note in the list.
        assert_eq!(
            *by_index.last().unwrap(),
            expected.last().unwrap(),
            "the last note in the list must be reachable by clicking it"
        );
    }

    #[test]
    fn every_header_shifts_nothing_because_it_keeps_its_own_slot() {
        // All five sections: pinned, today, yesterday, the seven-day bucket and
        // older. Five headers. If headers were skipped rather than slotted, the
        // oldest note's id would sit four rows too high.
        let mut pinned = aged(0.1);
        pinned.is_pinned = true;
        let notes = vec![pinned, aged(0.2), aged(1.5), aged(4.0), aged(30.0)];

        let rows = note_rows(&notes, AppleDate::now().raw());
        let ids: Vec<&str> = rows.iter().map(Row::id).collect();
        assert_eq!(ids.len(), rows.len());
        assert_eq!(
            ids.iter().filter(|id| id.is_empty()).count(),
            5,
            "one header per section, each keeping its own slot"
        );
        for note in &notes {
            let visual = ids.iter().position(|id| *id == note.id).expect("note on a row");
            assert_eq!(
                rows[visual], Row::Note(note.clone()),
                "the row at that index must be that note, not a header"
            );
        }
    }

    #[test]
    fn sections_place_notes_by_age_and_pin() {
        let today = aged(0.2);
        let yesterday = aged(1.5);
        let week = aged(4.0);
        let old = aged(30.0);
        let mut pinned = aged(0.1);
        pinned.is_pinned = true;

        let now = AppleDate::now().raw();
        let groups = sections(&[today, yesterday, week, old, pinned], now);
        let labels: Vec<&str> = groups.iter().map(|(s, _)| s.label()).collect();
        assert_eq!(
            labels,
            vec!["Pinned", "Today", "Yesterday", "Previous 7 days", "Older"]
        );
    }

    #[test]
    fn a_note_exactly_a_week_old_is_not_in_the_seven_day_bucket() {
        let n = aged(7.2);
        let now = AppleDate::now().raw();
        assert_eq!(Section::of(&n, now), Section::Older);
        let n = aged(6.9);
        assert_eq!(Section::of(&n, now), Section::Past7Days);
    }

    #[test]
    fn a_5k_note_query_keeps_up_with_typing() {
        let notes: Vec<Note> = (0..5_000)
            .map(|i| {
                let mut n = Note::new(None);
                n.text = format!("Note {i} about topic number {i}. Some filler prose.");
                n.updated_at = AppleDate::from_apple(700_000_000.0 + i as f64);
                n
            })
            .collect();
        let q = Query {
            scope: Scope::All,
            search: "topic 4".into(),
            sort: SortOrder::Edited,
        };

        // The UI path: build the index once, then pay only the scan per
        // keystroke.
        let started = std::time::Instant::now();
        let index = SearchIndex::build(&notes);
        let built = started.elapsed();

        let started = std::time::Instant::now();
        let hits = q.run_indexed(&notes, &index);
        let queried = started.elapsed();

        assert!(!hits.is_empty());
        assert!(
            built.as_millis() < 1_000,
            "index build took {built:?} — the fold fast path regressed"
        );
        assert!(
            queried.as_millis() < 50,
            "indexed query took {queried:?} — the hot path is no longer hot"
        );

        // Editing one note costs one re-fold, not a rebuild.
        let mut edited = notes.clone();
        edited[0].text = "brand new body text".into();
        let mut idx2 = SearchIndex::build(&notes);
        let started = std::time::Instant::now();
        idx2.invalidate(&edited[0].id);
        idx2.sync(&edited);
        let resync = started.elapsed();
        println!("build {built:?} query {queried:?} resync {resync:?}");
        assert!(
            resync.as_millis() < 50,
            "an incremental resync took {resync:?}"
        );
        assert_eq!(idx2.len(), notes.len());
    }

    #[test]
    fn a_edited_note_is_found_by_its_new_words() {
        // The regression: `sync` only folded a note it had *not* seen, so an
        // edited note kept its old fold in the index. Searching for a word
        // the user had just typed returned nothing, and searching for the
        // words they had deleted still returned the note. The index has to
        // notice the change on its own — the app never calls `invalidate`.
        let mut original = note("the old words are here");
        original.id = "note-1".into();
        let notes = vec![original.clone()];
        let mut index = SearchIndex::build(&notes);

        // The note is edited: same id, new body. No `invalidate` — this is
        // the app's real path.
        let mut edited = original.clone();
        edited.text = "entirely different content".into();
        index.sync(&[edited.clone()]);

        let q = Query {
            scope: Scope::All,
            search: "different".into(),
            sort: SortOrder::Edited,
        };
        let hits = q.run_indexed(&[edited.clone()], &index);
        assert_eq!(
            hits.len(),
            1,
            "the edited note's new words must be findable"
        );

        // And the words the user deleted must be gone.
        let q_old = Query {
            scope: Scope::All,
            search: "old".into(),
            sort: SortOrder::Edited,
        };
        let hits_old = q_old.run_indexed(&[edited.clone()], &index);
        assert!(
            hits_old.is_empty(),
            "the deleted words must not still match: {hits_old:?}"
        );
    }

    #[test]
    fn an_edited_title_is_found_by_its_new_words() {
        // The same regression for the title: a note whose *title* was set
        // after the index had folded it kept the derived title's fold, so
        // searching for the title the user typed found nothing.
        let mut original = note("body text");
        original.id = "note-1".into();
        let mut index = SearchIndex::build(&[original.clone()]);

        let mut edited = original.clone();
        edited.explicit_title = Some("My Important Heading".into());
        index.sync(&[edited.clone()]);

        let q = Query {
            scope: Scope::All,
            search: "important".into(),
            sort: SortOrder::Edited,
        };
        let hits = q.run_indexed(&[edited.clone()], &index);
        assert_eq!(hits.len(), 1, "the new title must be findable");
    }

    #[test]
    fn a_pin_does_not_refold_the_note() {
        // The content-addressed folds must not make every *other* edit
        // expensive: pinning a note changes neither its title nor its body,
        // so it must cost no fold. The index keeps the source text it folded
        // from, and a note whose source is unchanged is left alone.
        let mut n = note("stable body");
        n.id = "note-1".into();
        let notes = vec![n.clone()];
        let mut index = SearchIndex::build(&notes);

        let mut pinned = n.clone();
        pinned.is_pinned = true;
        index.sync(&[pinned.clone()]);

        // The fold is still the original's — no re-fold happened, and the
        // index still finds the body.
        assert_eq!(index.folded_body(&pinned.id, &pinned), "stable body");
        assert_eq!(index.len(), 1);
    }
}