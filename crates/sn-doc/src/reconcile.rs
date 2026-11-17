//! Reconciling two readings of the same page into one coherent text.
//!
//! Two engines can read one PDF page: the text layer the producer baked
//! in, and this app's own recognition of the raster. They read the same
//! words — usually the *same* words — and the difference is worth nothing
//! unless it is handled at the word the engines disagree about.
//!
//! The reader that came before this spliced whole lines: a recognition
//! line that no single layer line contained was appended verbatim after
//! the layer's text. But the two readings wrap, split, spell and *order*
//! the same line differently (`maternal and fetal risks` vs `maternal
//! :and fetal rrisks`, `IUGR` vs `TUGR`, one recognition line spanning two
//! layer lines, a footer read as `14 16-Jun-21` where the layer has
//! `16-Jun-21` then `14`), so the containment test failed and the page
//! said everything twice — the layer's copy, then the recognition's copy
//! of the same sentences underneath it. Measured on the user's own
//! library: every page of a scanned textbook logged `the OCR mix added N
//! chars` on every read, and what the reader spoke was the same page read
//! twice.
//!
//! [`reconcile`] replaces that. One reading **leads** (the caller chooses
//! it); the other only fills its gaps. Each secondary line's tokens are
//! matched, in their longest order-preserving chain, against the primary's
//! whole page — against the stream, not against one line, because the two
//! readings wrap the same sentence differently. The unmatched remainder
//! decides the line's fate:
//!
//! * **Nothing unmatched** — the sentence is already on the page. Nothing
//!   is added, and the matched primary tokens are consumed, so a sentence
//!   the secondary says twice is only covered once: the second copy
//!   survives.
//! * **Unmatched words the primary has never said** (and the chain still
//!   covers most of the line) — the same sentence with words missing: the
//!   missing words are spliced into the line they belong to, at the
//!   position they occupy in the secondary reading. The primary's own
//!   punctuation, spacing and line breaks survive: only insertions are
//!   made, never a rebuild.
//! * **Anything else** — genuinely new content; the line is appended
//!   verbatim after a blank line, as before.
//!
//! **The page's own repetition is content, not noise.** Deduplication is
//! only ever *between* the readings, never within one: the primary's
//! lines are never deleted or merged, so a sentence the page legitimately
//! says twice keeps both copies, and a sentence the secondary says more
//! often than the primary holds it appends the extra copy rather than
//! swallowing it.
//!
//! Two tolerances keep an engine's accidents from becoming words. A token
//! the primary says *with a one-character misspelling* (`rrisks` for
//! `risks`, `TUGR` for `IUGR`) counts as present — without the tolerance,
//! the recogniser's every typo would drag its whole line into the page
//! again. And a token within two edits of a word the page already says
//! (`catJse` for `cause`) is a corruption of something present, not a
//! word the page is missing, so it is never spliced in. A token mixing
//! letters with digits (`n1ay-be`, `b0wel`, `P0P` — how a producer's
//! shifted OCR writes "maybe", "bowel", "POP") is not a word at all.

use std::collections::HashMap;

/// How much of a secondary line must match the primary before its
/// unmatched words are spliced in rather than the line appended whole.
/// Below it the line is somebody else's sentence: a genuine addition is
/// appended, and a near-miss of a sentence the primary does not have is
/// not torn apart to look like one. A long line with a strong chain (three
/// or more words already matched in order) earns a lower bar — four
/// matched words are evidence enough that the readings agree, where one
/// matched word is not.
const SPLICE_RATIO: f64 = 0.6;
const SPLICE_RATIO_LONG: f64 = 0.4;
const SPLICE_LONG_CHAIN: usize = 3;

/// How many words one secondary line may have spliced into the primary.
/// Gap-filling is for the same sentence with a word or two missed; a line
/// with four or more words the primary has never said is somebody else's
/// sentence, and it is appended whole instead of sprinkled across the
/// page (a chain that spans several of the primary's lines would land its
/// missing words in whichever line each anchor lived in — measured on the
/// user's page 725, which produced a scramble).
const MAX_SPLICE_TOKENS: usize = 3;

/// One word of a line: its text, its comparison key, and where it sits in
/// the line (character offsets) — an inserted word is spliced in at an
/// offset, so the line's own punctuation and spacing survive.
#[derive(Debug, Clone)]
struct Token {
    raw: String,
    key: String,
    start: usize,
    end: usize,
}

/// Split a line into its alphanumeric tokens with character offsets.
fn tokenise(line: &str) -> Vec<Token> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_alphanumeric() {
            let start = i;
            while i < chars.len() && chars[i].is_alphanumeric() {
                i += 1;
            }
            let raw: String = chars[start..i].iter().collect();
            out.push(Token {
                key: raw.to_lowercase(),
                raw,
                start,
                end: i,
            });
        } else {
            i += 1;
        }
    }
    out
}

/// `key`'s lookup set: itself plus every one-character deletion — the
/// neighbourhood a key within one edit of it must touch.
fn variants(key: &str) -> Vec<String> {
    let chars: Vec<char> = key.chars().collect();
    let mut out = vec![key.to_string()];
    for i in 0..chars.len() {
        out.push(chars
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(_, c)| c)
            .collect());
    }
    out
}

/// Whether two comparison keys are the same word to within one edit — a
/// substitution, a transposition, an insertion or a deletion. Short keys
/// are compared exactly: "the"/"teh" may differ by one edit but so do
/// "of"/"or", and a two-letter word's tolerance buys noise.
fn within_one_edit(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    if a.chars().count() < 3 || b.chars().count() < 3 {
        return false;
    }
    let x: Vec<char> = a.chars().collect();
    let y: Vec<char> = b.chars().collect();
    match x.len().abs_diff(y.len()) {
        0 => {
            let diffs: Vec<usize> = (0..x.len()).filter(|i| x[*i] != y[*i]).collect();
            match diffs.len() {
                1 => true, // one substitution
                2 => diffs[1] == diffs[0] + 1
                    && x[diffs[0]] == y[diffs[1]]
                    && x[diffs[1]] == y[diffs[0]], // one transposition
                _ => false,
            }
        }
        1 => {
            // One insertion or deletion: the longer becomes the shorter by
            // dropping exactly one character.
            let (long, short) = if x.len() > y.len() { (&x, &y) } else { (&y, &x) };
            let mut skipped = false;
            let mut li = 0;
            for sc in short {
                while li < long.len() && long[li] != *sc {
                    if skipped {
                        return false;
                    }
                    skipped = true;
                    li += 1;
                }
                li += 1;
            }
            true
        }
        _ => false,
    }
}

/// Bounded Levenshtein: whether two keys are within `limit` edits. Only
/// asked for the corruption guard (a token that is *almost* a word the
/// page already says), so the full table is fine — the keys are words.
fn within_n_edits(a: &str, b: &str, limit: usize) -> bool {
    let x: Vec<char> = a.chars().collect();
    let y: Vec<char> = b.chars().collect();
    if x.len().abs_diff(y.len()) > limit {
        return false;
    }
    let mut prev: Vec<usize> = (0..=y.len()).collect();
    let mut cur = vec![0usize; y.len() + 1];
    for i in 1..=x.len() {
        cur[0] = i;
        let mut row_min = cur[0];
        for j in 1..=y.len() {
            let sub = prev[j - 1] + usize::from(x[i - 1] != y[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(sub);
            row_min = row_min.min(cur[j]);
        }
        if row_min > limit {
            return false;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[y.len()] <= limit
}

/// A word that is a letter/digit mix is never a word the primary is
/// missing: `n1ay-be`, `b0wel`, `P0P` are how a producer's shifted OCR
/// writes "maybe", "bowel", "POP", and splicing them into a good reading
/// would be the junk speaking again.
fn is_wordlike(key: &str) -> bool {
    let has_alpha = key.chars().any(char::is_alphabetic);
    let has_digit = key.chars().any(|c| c.is_ascii_digit());
    if has_alpha && has_digit {
        return false;
    }
    !(has_alpha && key.chars().count() < 3)
}

/// The primary reading as a consumable token stream: every word it says,
/// in order, indexed for the one-edit neighbourhood so a secondary token
/// finds its matches in a handful of hash lookups instead of a scan.
#[derive(Default)]
struct Stream {
    /// Token id → (line id, key). The line id ties a match back to the
    /// line a missing word gets spliced into; the key repeats the token's
    /// comparison key for the one-edit confirmation.
    tokens: Vec<(usize, String)>,
    /// Token id → (start, end) character offsets in its own line.
    spans: Vec<(usize, usize)>,
    /// Token id → consumed. Consumption is what keeps the page's own
    /// repetition: a sentence is covered only while it still has
    /// unconsumed copies to match against.
    used: Vec<bool>,
    /// Exact key → token ids (ascending).
    exact: HashMap<String, Vec<usize>>,
    /// Deletion variant → token ids whose key has that variant.
    neighbourhood: HashMap<String, Vec<usize>>,
}

impl Stream {
    fn new(primary_lines: &[&str]) -> Self {
        let mut s = Stream::default();
        for (line_id, line) in primary_lines.iter().enumerate() {
            for token in tokenise(line) {
                let id = s.tokens.len();
                s.exact.entry(token.key.clone()).or_default().push(id);
                for v in variants(&token.key) {
                    s.neighbourhood.entry(v).or_default().push(id);
                }
                s.tokens.push((line_id, token.key));
                s.spans.push((token.start, token.end));
                s.used.push(false);
            }
        }
        s
    }

    /// Token ids saying `key` — exactly, or as a one-edit misspelling of
    /// something the stream says — confirmed with the exact test.
    fn matches(&self, key: &str) -> Vec<usize> {
        let mut ids: Vec<usize> = self.exact.get(key).cloned().unwrap_or_default();
        for v in variants(key) {
            if let Some(more) = self.neighbourhood.get(&v) {
                ids.extend_from_slice(more);
            }
        }
        ids.sort_unstable();
        ids.dedup();
        ids.into_iter()
            .filter(|id| self.tokens[*id].1 == key || within_one_edit(key, &self.tokens[*id].1))
            .collect()
    }

    /// Unconsumed token ids saying `key`, ascending — the positions a
    /// match chain may step onto.
    fn candidates(&self, key: &str) -> Vec<usize> {
        self.matches(key).into_iter().filter(|id| !self.used[*id]).collect()
    }

    /// Whether the stream still holds an unconsumed copy of `key`
    /// anywhere — the test that lets a re-wrapped footer (`14 16-Jun-21`
    /// where the layer has the same words the other way round) count as
    /// covered rather than appended.
    fn holds(&self, key: &str) -> Option<usize> {
        self.matches(key).into_iter().find(|id| !self.used[*id])
    }

    /// Whether the stream says `key`, spent or not.
    fn says(&self, key: &str) -> bool {
        !self.matches(key).is_empty()
    }

    /// Whether the stream says something within two edits of `key` — a
    /// corruption (`catJse` for `cause`) rather than a new word. One edit
    /// is already covered by [`Self::says`]; this is the wider net a
    /// shifted OCR's artefacts fall into.
    fn corrupts(&self, key: &str) -> bool {
        if key.chars().count() < 5 {
            return false; // the short words' tolerance stays at one edit
        }
        self.tokens
            .iter()
            .any(|(_, other)| within_n_edits(key, other, 2))
    }

    /// Whether `key` is two of the stream's words run together — the two
    /// engines segmenting the same text differently (`multidisciplinarymanagement`
    /// for "multidisciplinary management", `pulmonaryhypertension` for
    /// "pulmonary hypertension"). Both halves on the page means the word
    /// is content the page already has, not a word it is missing; splicing
    /// it in would say the sentence twice.
    fn fusion(&self, key: &str) -> bool {
        let chars: Vec<char> = key.chars().collect();
        if chars.len() < 8 {
            return false; // both halves at least four characters
        }
        for split in 4..chars.len() - 3 {
            let a: String = chars[..split].iter().collect();
            let b: String = chars[split..].iter().collect();
            if self.says(&a) && self.says(&b) {
                return true;
            }
        }
        false
    }

    fn consume(&mut self, id: usize) {
        self.used[id] = true;
    }
}

/// One secondary line's verdict against the primary stream.
enum Verdict {
    /// Every word accounted for: the sentence is already on the page.
    Covered,
    /// Most of the chain matched; these are the words the primary is
    /// missing, with the matches they sit between.
    Splice { matches: Vec<(usize, usize)>, missing: Vec<usize> },
    /// Somebody else's sentence: append the line whole.
    Append,
}

/// Judge one secondary line. The match is the longest order-preserving
/// chain of unconsumed stream tokens the line's tokens can step onto —
/// longest, not first-fit, because the two readings reorder footers and
/// columns and a greedy walk would strand the rest of the line behind a
/// footer read the other way round.
fn judge(stream: &mut Stream, tokens: &[Token]) -> Verdict {
    // candidates[sj]: the unconsumed stream positions token sj may match.
    let candidates: Vec<Vec<usize>> = tokens.iter().map(|t| stream.candidates(&t.key)).collect();
    let m = tokens.len();
    // ends[sj]: the longest chain ending with token sj matched —
    // (length, stream id, index of the token it extends), for backtracking.
    let mut ends: Vec<Option<(u32, usize, usize)>> = vec![None; m];
    let mut best = (0u32, usize::MAX);
    for i in 0..m {
        for &p in &candidates[i] {
            let mut len = 1u32;
            let mut parent = usize::MAX;
            for j in 0..i {
                if let Some((l, last, _)) = ends[j] {
                    if last < p && l + 1 > len {
                        len = l + 1;
                        parent = j;
                    }
                }
            }
            if ends[i].is_none_or(|(l, _, _)| len > l) {
                ends[i] = Some((len, p, parent));
                if len > best.0 {
                    best = (len, i);
                }
            }
        }
    }
    // Backtrack the best chain into (stream id, secondary index) pairs.
    let mut chain: Vec<(usize, usize)> = Vec::new();
    let mut i = best.1;
    while i != usize::MAX {
        if let Some((_, p, parent)) = ends[i] {
            chain.push((p, i));
            i = if parent == usize::MAX { usize::MAX } else { parent };
        } else {
            break;
        }
    }
    chain.reverse();
    // Classify the unmatched tokens: a word the page still holds an
    // unconsumed copy of is spent where it lies (the line is a re-wrapping
    // of content the primary has); a word the page says but has spent is
    // an extra copy arriving; a word the page has never said may be
    // spliced in, unless it is only a corruption of a word it has.
    let matched: Vec<usize> = chain.iter().map(|(_, sj)| *sj).collect();
    let mut missing: Vec<usize> = Vec::new();
    for (sj, t) in tokens.iter().enumerate() {
        if matched.contains(&sj) || !is_wordlike(&t.key) {
            continue;
        }
        if let Some(id) = stream.holds(&t.key) {
            stream.consume(id);
        } else if stream.says(&t.key) || stream.corrupts(&t.key) || stream.fusion(&t.key) {
            // Already on the page: an extra copy, a corrupted repeat, or
            // the same words with a different segmentation.
        } else {
            missing.push(sj);
        }
    }
    if missing.is_empty() {
        if chain.is_empty() {
            // Every word this line says is already spent on the page: the
            // sentence arrived a second time, and the page's repetition is
            // content — the extra copy is appended, not swallowed.
            return Verdict::Append;
        }
        for (id, _) in &chain {
            stream.consume(*id);
        }
        return Verdict::Covered;
    }
    let ratio = chain.len() as f64 / m as f64;
    let bar = if chain.len() >= SPLICE_LONG_CHAIN { SPLICE_RATIO_LONG } else { SPLICE_RATIO };
    // The gap-filling guards: few words to add, and all of them anchored
    // into one line — the same sentence with a word or two missing. A
    // longer list of missing words (or anchors scattered over several of
    // the primary's lines) is a different reading of the page, and it is
    // appended whole rather than sprinkled in.
    let target = |sj: usize| -> usize {
        match matches_next(&chain, sj).or_else(|| matches_prev(&chain, sj)) {
            Some(id) => stream.tokens[id].0,
            None => usize::MAX,
        }
    };
    let one_line = missing
        .iter()
        .map(|sj| target(*sj))
        .all(|line| line == target(missing[0]));
    if !chain.is_empty()
        && ratio >= bar
        && missing.len() <= MAX_SPLICE_TOKENS
        && one_line
    {
        for (id, _) in &chain {
            stream.consume(*id);
        }
        return Verdict::Splice { matches: chain, missing };
    }
    Verdict::Append
}

/// The next chain match after secondary position `sj` — the token a
/// missing word is inserted before.
fn matches_next(chain: &[(usize, usize)], sj: usize) -> Option<usize> {
    chain.iter().find(|(_, other)| *other > sj).map(|(id, _)| *id)
}

/// The last chain match before secondary position `sj` — the token a
/// missing word is inserted after when nothing follows it.
fn matches_prev(chain: &[(usize, usize)], sj: usize) -> Option<usize> {
    chain.iter().rev().find(|(_, other)| *other < sj).map(|(id, _)| *id)
}

/// Splice the secondary line's missing words into the primary lines, at
/// the positions the matches imply. Returns the edits as
/// `(line id, char offset, order, text)` — offsets in the *original* line,
/// so a page's edits can all be applied together, last offset first.
fn splice_edits(
    stream: &Stream,
    secondary: &[Token],
    matches: &[(usize, usize)],
    missing: &[usize],
) -> Vec<(usize, usize, usize, String)> {
    let mut edits = Vec::new();
    for &sj in missing {
        let t = &secondary[sj];
        // Insert before the next token the two readings share, or after
        // the previous one — either way into the line that match lives in.
        let (next, prev) = (matches_next(matches, sj), matches_prev(matches, sj));
        let (line, offset, text) = match (next, prev) {
            (Some(id), _) => {
                let (line, _) = &stream.tokens[id];
                (*line, stream.spans[id].0, format!("{} ", t.raw))
            }
            (None, Some(id)) => {
                let (line, _) = &stream.tokens[id];
                (*line, stream.spans[id].1, format!(" {}", t.raw))
            }
            (None, None) => continue,
        };
        edits.push((line, offset, sj, text));
    }
    edits
}

/// What [`reconcile`] did to a page — the diagnostics behind the reader's
/// log line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReconcileReport {
    /// Secondary lines appended whole: genuinely new content.
    pub appended_lines: usize,
    /// Words spliced into the primary's lines: the gaps filled.
    pub spliced_words: usize,
}

/// Reconcile two readings of the same page into one.
///
/// `primary` is the reading that leads — the one the reader speaks first,
/// in its own order, with its own punctuation and line breaks. `secondary`
/// fills its gaps and nothing else. The result is never shorter than the
/// primary, and adds nothing the primary already says.
pub fn reconcile(primary: &str, secondary: &str) -> String {
    reconcile_reported(primary, secondary).0
}

/// [`reconcile`] with a count of what it added, for the diagnostics and
/// the tests.
pub fn reconcile_reported(primary: &str, secondary: &str) -> (String, ReconcileReport) {
    if secondary.trim().is_empty() {
        return (primary.to_string(), ReconcileReport { appended_lines: 0, spliced_words: 0 });
    }
    if primary.trim().is_empty() {
        return (
            secondary.to_string(),
            ReconcileReport {
                appended_lines: secondary.lines().filter(|l| !l.trim().is_empty()).count(),
                spliced_words: 0,
            },
        );
    }
    let primary_lines: Vec<&str> = primary.lines().collect();
    let mut stream = Stream::new(&primary_lines);
    let mut edits: Vec<(usize, usize, usize, String)> = Vec::new();
    let mut extra: Vec<String> = Vec::new();
    let mut report = ReconcileReport { appended_lines: 0, spliced_words: 0 };
    for line in secondary.lines() {
        let tokens = tokenise(line);
        if tokens.is_empty() {
            continue;
        }
        match judge(&mut stream, &tokens) {
            Verdict::Covered => {
                // The sentence is on the page already, in the primary's own
                // wrapping; `judge` spent its copy so a second one the
                // secondary saw is not swallowed.
            }
            Verdict::Splice { matches, missing } => {
                report.spliced_words += missing.len();
                edits.extend(splice_edits(&stream, &tokens, &matches, &missing));
            }
            Verdict::Append => {
                let text = line.trim_end();
                if !text.is_empty() {
                    report.appended_lines += 1;
                    extra.push(text.to_string());
                }
            }
        }
    }
    (apply(primary, &primary_lines, &edits, &extra), report)
}

/// Build the reconciled page: the primary's lines with their splices, then
/// the appended lines after a blank line — the shape the reader, the
/// read-along frontier and the bookmarks already consume.
fn apply(
    primary: &str,
    primary_lines: &[&str],
    edits: &[(usize, usize, usize, String)],
    extra: &[String],
) -> String {
    if edits.is_empty() {
        if extra.is_empty() {
            // The primary stands byte-for-byte: offsets into it (a
            // read-along frontier, a bookmark) keep pointing where they did.
            return primary.to_string();
        }
        let mut result = String::from(primary);
        if !result.ends_with('\n') {
            result.push('\n');
        }
        result.push('\n');
        result.push_str(&extra.join("\n"));
        if primary.ends_with('\n') {
            result.push('\n');
        }
        return result;
    }
    let mut lines: Vec<String> = primary_lines.iter().map(|s| (*s).to_string()).collect();
    // Per line, last offset first (and same-offset edits in reverse
    // secondary order), so every splice lands at an offset that is still
    // valid — the later ones have not moved yet.
    let mut by_line: HashMap<usize, Vec<(usize, usize, String)>> = HashMap::new();
    for (line, offset, order, text) in edits {
        by_line
            .entry(*line)
            .or_default()
            .push((*offset, *order, text.clone()));
    }
    for (line, mut items) in by_line {
        items.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        let mut chars: Vec<char> = lines[line].chars().collect();
        for (offset, _, text) in items {
            let at = offset.min(chars.len());
            chars.splice(at..at, text.chars());
        }
        lines[line] = chars.into_iter().collect();
    }
    let mut result = lines.join("\n");
    if !extra.is_empty() {
        if !result.ends_with('\n') {
            result.push('\n');
        }
        result.push('\n');
        result.push_str(&extra.join("\n"));
    }
    if primary.ends_with('\n') {
        result.push('\n');
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_secondary_changes_nothing() {
        let page = "maternal and fetal risks\n\nfetal complications:";
        assert_eq!(reconcile(page, ""), page);
        assert_eq!(reconcile(page, "  \n "), page);
        assert_eq!(reconcile(page, page), page);
    }

    #[test]
    fn a_word_the_primary_missed_is_spliced_in_place() {
        // The recognition read a word the layer never extracted. It goes
        // into the line it belongs to, not into a block at the page's end.
        let out = reconcile("the cervical plug is lost", "the cervical plug is truly lost");
        assert_eq!(out, "the cervical plug is truly lost");
    }

    #[test]
    fn a_splice_keeps_the_lines_punctuation() {
        let out = reconcile(
            "Assess functional class of heart disease.",
            "Assess the functional class of heart disease.",
        );
        assert_eq!(out, "Assess the functional class of heart disease.");
    }

    #[test]
    fn a_misspelling_is_not_a_word_the_primary_is_missing() {
        // Page 388 of the user's scanned textbook, verbatim: the layer and
        // the recognition disagree by a shifted character, and the old
        // merge appended the recognition's whole line for it.
        let layer = "maternal and fetal risks\n\nfetal complications:\n\n\
                     • spontaneous miscarriage\n\n• IUGR\n\n• preterm birth\n\n\
                     • fetal hypoxia\n\n• intrauterine death\n\n\
                     • congenital heart anomalies\n\n16-Jun-21\n\n14";
        let ocr = "maternal :and fetal rrisks\n\nfetal complications:\n\n\
                   spontaneous miscarriage\n\nTUGR\n\npreterm birth fetal hypoxia\n\n\
                   intrauterine death\n\ncongenital heart anomalies\n\n14 16-Jun-21";
        let out = reconcile(layer, ocr);
        assert_eq!(out, layer, "nothing the layer says came in a second time: {out:?}");
        assert!(!out.contains("rrisks"), "a typo never becomes a new word");
        assert!(!out.contains("TUGR"), "a misspelling never becomes a new word");
    }

    #[test]
    fn a_sentence_spread_over_two_lines_is_still_covered() {
        // The recognition wraps the same sentence differently: one line
        // spanning two layer lines. Its tokens all match the stream, so it
        // adds nothing — not a third copy of the sentence's words.
        let layer = "Management of breech presentation\n\nTerm breech trial, Lancet 2000";
        let ocr = "Management of breech\npresentation\nTerm breech trial, Lancet 2000";
        assert_eq!(reconcile(layer, ocr), layer);
    }

    #[test]
    fn a_line_the_primary_never_said_is_appended() {
        let layer = "Management of breech presentation";
        let ocr = "Management of breech presentation\n\nFIG. 4. The Frank breech position.";
        let out = reconcile(layer, ocr);
        assert!(out.contains("FIG. 4. The Frank breech position."), "{out:?}");
        assert_eq!(out.matches("Management of breech").count(), 1, "no duplication");
    }

    #[test]
    fn a_page_that_says_a_sentence_twice_keeps_both_copies() {
        // The page's own repetition is content, not noise: deduplication is
        // only ever between the readings, never within one.
        let layer = "Do not resuscitate.\n\nDo not resuscitate.";
        let ocr = "Do not resuscitate.";
        assert_eq!(reconcile(layer, ocr), layer, "the page's second copy stands");
    }

    #[test]
    fn a_sentence_the_primary_says_once_and_the_secondary_twice_gains_a_copy() {
        // And the mirror: the recognition read the page's repeat that the
        // layer missed. The page says it twice — so the reading must too.
        let layer = "Check the fetal heart.";
        let ocr = "Check the fetal heart.\n\nCheck the fetal heart.";
        let out = reconcile(layer, ocr);
        assert_eq!(out.matches("Check the fetal heart").count(), 2, "{out:?}");
    }

    #[test]
    fn shifted_ocr_junk_cannot_leak_into_a_good_reading() {
        // A token mixing letters and digits is never a word the primary is
        // missing — it is how a shifted OCR writes a real word. And
        // `catJse` is a corruption of `cause`, which the page already says.
        let layer = "may be the cause of bladder and bowel dysfunction";
        let ocr = "n1ay-be the catJse of bladder, b0wel";
        assert_eq!(reconcile(layer, ocr), layer);
    }

    #[test]
    fn a_footer_read_in_the_other_order_is_still_the_same_words() {
        // The recognition merged the page number into the date line, the
        // other way round from the layer. Same words, no new content.
        let layer = "congenital heart anomalies\n\n16-Jun-21\n\n14";
        let ocr = "congenital heart anomalies\n\n14 16-Jun-21";
        assert_eq!(reconcile(layer, ocr), layer);
    }

    #[test]
    fn the_primary_leads_even_when_the_secondary_is_longer() {
        // The caller chose the primary; the result keeps its order and its
        // words first, with the secondary's genuinely-new line after them.
        let primary = "first the layer's own reading order";
        let secondary = "first the layer's own reading order\n\nand the recognition's extra line";
        let out = reconcile(primary, secondary);
        assert!(out.starts_with(primary), "{out:?}");
        assert!(out.contains("and the recognition's extra line"));
    }

    #[test]
    fn an_empty_primary_takes_the_secondary_whole() {
        assert_eq!(
            reconcile("", "the recognition read something"),
            "the recognition read something"
        );
    }

    #[test]
    fn a_trailing_newline_survives_an_appended_block() {
        // Byte offsets into the base are a bookmark's currency: the shape
        // the old merge produced (base, blank line, added block) is kept.
        let out = reconcile("base line\n", "base line\n\nadded line");
        assert!(out.starts_with("base line\n"), "{out:?}");
        assert!(out.contains("added line"));
    }

    #[test]
    fn a_two_edit_corruption_of_a_page_word_is_never_spliced() {
        // `catJse` is two edits from `cause`: not a new word, so it never
        // reaches the page — but a genuinely new long word still does.
        let stream = Stream::new(&["the cause of the failure"]);
        assert!(stream.corrupts("catJse".to_lowercase().as_str()));
        assert!(!stream.corrupts("platelet"));
        let out = reconcile(
            "the cause of the failure",
            "the catJse of the failure and the platelet count",
        );
        assert!(out.contains("platelet"), "a real new word is spliced: {out:?}");
        assert!(!out.contains("catJse"), "a corruption never lands: {out:?}");
    }

    #[test]
    fn a_fused_word_is_segmentation_not_content() {
        // Page 389 of the user's scanned book: the recognition ran two
        // words together ("multidisciplinarymanagement"), and splicing that
        // fusion into the layer's line said the words twice.
        let layer = "• multidisciplinary management with\n\nobstetrician and cardiologist.";
        let ocr = "multidisciplinarymanagement with obstetrician and cardiologist.";
        let (out, report) = reconcile_reported(layer, ocr);
        assert_eq!(out, layer, "the fused word is already on the page: {out:?}");
        assert_eq!(report.spliced_words, 0);
    }

    #[test]
    fn a_line_with_many_new_words_is_appended_not_scattered() {
        // Page 725: a chain across several of the primary's lines would
        // sprinkle the missing words into whichever line each anchor lived
        // in. A reading that differs this much is appended whole.
        let layer = "the presenting part is the vertex\n\nthe brow is rare\n\nface presentation";
        let ocr = "brow presentation it is the least common malpresentation in 1 in 2,000 labours \
                   with partial extension of the fetal head between the anterior fontanel";
        let (out, report) = reconcile_reported(layer, ocr);
        assert_eq!(report.spliced_words, 0, "nothing was sprinkled: {out:?}");
        assert!(out.contains("brow presentation it is the least common"), "appended whole: {out:?}");
    }
}
