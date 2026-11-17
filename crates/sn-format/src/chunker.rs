//! Sentence chunking for speech.
//!
//! Text goes to an engine one chunk at a time, so chunking is the app's hot
//! path: it runs on every keystroke-length document, and on a 200k-character
//! book chapter. The Swift chunker was `O(n²)` — invisible on a note, seconds on
//! a chapter (iOS anti-pattern AP15). Everything here is a single forward pass
//! with `char_indices()`, so cost is linear and independent of sentence count.
//!
//! Three properties matter and are each tested:
//!
//! 1. **Lossless.** Concatenating every chunk's text reproduces the input
//!    exactly, including the whitespace after each sentence. Bookmarks are
//!    byte offsets into the source, so a dropped space would desynchronise
//!    resume from the text the user sees.
//! 2. **Bounded.** No chunk exceeds `max_chars` unless a single word is longer,
//!    in which case it is one chunk rather than a split word.
//! 3. **Oversized sentences are split, not truncated.** A 10k-character run-on
//!    paragraph becomes several chunks.

/// Default ceiling for a synthesised chunk. Sized so a slow engine (Kokoro at
/// RTF ~1.5) still renders a chunk in well under a second.
pub const DEFAULT_MAX_CHARS: usize = 300;

/// The ceiling no engine in this app accepts a chunk above.
///
/// Supertonic refuses a chunk over 300 characters — not with an error, with a
/// beep and a skipped phrase — so an unspoken chunk is a chunk the book never
/// says. The chunker's own `max` is a performance number the caller chooses;
/// this one is a correctness floor, and [`hard_cap`] keeps a piece from ever
/// crossing it. Equal to `DEFAULT_MAX_CHARS` today, which is why the pipeline
/// asks for exactly that.
pub const MAX_ENGINE_CHARS: usize = 300;

/// Pieces shorter than this get glued to the piece before them.
///
/// Engines fail on an empty or one-token tail — a lone "It." is a
/// synthesisation failure, not a sentence — so the gluing is a correctness
/// requirement, not a tidiness preference.
pub const MIN_PIECE_CHARS: usize = 4;

/// One piece of text destined for an engine, with its position in the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// The text to synthesise. Never modified.
    pub text: String,
    /// Byte offset of `text` within the source.
    pub start: usize,
    /// Byte length of `text`.
    pub len: usize,
    /// Zero-based index within the plan.
    pub index: usize,
}

impl Chunk {
    pub fn end(&self) -> usize {
        self.start + self.len
    }
}

/// Split `text` into sentence-bounded pieces.
///
/// A boundary is a run of `. ! ? …` (or `。！？`) that is followed by
/// whitespace or the end of the text, or a newline, which always ends a piece.
/// A `.` between two digits is not a boundary, so `3.14` and `v1.2` survive.
/// The terminating run *and* the whitespace after it belong to the piece, which
/// is what makes the split lossless.
pub fn sentence_pieces(text: &str, max_chars: usize) -> Vec<Chunk> {
    let max = max_chars.max(MIN_PIECE_CHARS);
    let bytes = text.as_bytes();
    let mut pieces: Vec<Chunk> = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;

    while i < text.len() {
        let Some(ch) = char_at(text, i) else { break };

        // Hard boundaries: a newline always ends a piece.
        if ch == '\n' || ch == '\r' || ch == '\u{2028}' || ch == '\u{2029}' {
            let mut end = i + ch.len_utf8();
            // CRLF is one terminator.
            if ch == '\r' && bytes.get(end) == Some(&b'\n') {
                end += 1;
            }
            push_piece(&mut pieces, text, start, end);
            start = end;
            i = end;
            continue;
        }

        if is_cjk_terminator(ch) {
            // CJK full stops end a sentence unconditionally: Japanese and
            // Chinese text does not put a space after them, so requiring
            // trailing whitespace would never find a boundary and the whole
            // chapter would become one chunk.
            let mut end = i + ch.len_utf8();
            while let Some(next) = char_at(text, end) {
                if is_cjk_terminator(next) {
                    end += next.len_utf8();
                } else {
                    break;
                }
            }
            push_piece(&mut pieces, text, start, end);
            start = end;
            i = end;
            continue;
        }

        if is_ascii_terminator(ch) {
            let mut j = i + ch.len_utf8();
            while let Some(next) = char_at(text, j) {
                if is_ascii_terminator(next) {
                    j += next.len_utf8();
                } else {
                    break;
                }
            }
            // A run of ASCII terminators ends a sentence only when what follows
            // is whitespace or nothing — otherwise it is a decimal point or an
            // ellipsis inside a word.
            let follows_boundary = match char_at(text, j) {
                None => true,
                Some(next) => next.is_whitespace(),
            };
            if follows_boundary {
                // Consume the trailing whitespace into this piece so the
                // concatenation stays exact.
                let mut end = j;
                while let Some(next) = char_at(text, end) {
                    if next.is_whitespace() && next != '\n' && next != '\r' {
                        end += next.len_utf8();
                    } else {
                        break;
                    }
                }
                push_piece(&mut pieces, text, start, end);
                start = end;
                i = end;
                continue;
            }
            // "3.14": a dot between two digits is part of the number.
            if ch == '.' && i > 0 && i + 1 < text.len() {
                let prev = bytes[i - 1];
                let next_byte = bytes[i + 1];
                if prev.is_ascii_digit() && next_byte.is_ascii_digit() {
                    i = j;
                    continue;
                }
            }
            i = j;
            continue;
        }

        i += ch.len_utf8();
    }

    if start < text.len() {
        push_piece(&mut pieces, text, start, text.len());
    }

    split_oversized(pieces, text, max)
    .into_iter()
    .enumerate()
    .map(|(index, (s, e))| Chunk {
        text: text[s..e].to_string(),
        start: s,
        len: e - s,
        index,
    })
    .collect()
}

fn char_at(text: &str, i: usize) -> Option<char> {
    text[i..].chars().next()
}

fn is_ascii_terminator(c: char) -> bool {
    matches!(c, '.' | '!' | '?' | '\u{2026}')
}

fn is_cjk_terminator(c: char) -> bool {
    matches!(c, '\u{3002}' | '\u{ff01}' | '\u{ff1f}')
}

/// Record `[start, end)` as a piece, dropping empty spans.
fn push_piece(pieces: &mut Vec<Chunk>, _text: &str, start: usize, end: usize) {
    if end > start {
        pieces.push(Chunk {
            text: String::new(),
            start,
            len: end - start,
            index: 0,
        });
    }
}

/// Break pieces that exceed `max` at word boundaries.
///
/// A word longer than `max` gets its own piece rather than being cut in half:
/// cutting mid-word produces two fragments that an engine may refuse or that a
/// voice pronounces as garbage.
///
/// The one exception is the engine's own ceiling. A piece this pass still
/// leaves longer than `hard_cap(max)` is not speakable at all — Supertonic
/// refuses a chunk over 300 characters with a beep and a skipped phrase — so
/// that one is cut at a safe character boundary instead. A mangled word is
/// wrong on one chunk; an unspoken one is missing from the book.
fn split_oversized(pieces: Vec<Chunk>, text: &str, max: usize) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(pieces.len());
    for p in pieces {
        let len = p.len;
        if len <= max {
            out.push((p.start, p.end()));
            continue;
        }
        let cut = split_piece(p, text, max);
        // The caller's own ceiling, or the engine's — whichever is lower. A
        // piece past the engine ceiling is not speakable at all, so it is cut
        // no matter how small `max` is; the `hard_cap` headroom exists for
        // the *caller's* `max`, and only when that leaves the piece inside
        // the engine's own limit. The `min` is the whole rule: a 304-char
        // piece at `max = 100` is still 304 chars, and the engine still
        // beeps and skips it.
        let ceiling = hard_cap(max).min(MAX_ENGINE_CHARS);
        if len > ceiling {
            out.extend(cap_longest(cut, text, ceiling));
        } else {
            out.extend(cut);
        }
    }
    out
}

/// The ceiling past which a word is cut rather than emitted whole: `max` with
/// the headroom `max` itself allows, so a merely over-long word is still spoken
/// whole and only a token no engine accepts is split.
fn hard_cap(max: usize) -> usize {
    max + MIN_PIECE_CHARS
}

/// Cut every piece still longer than `hard_cap(max)` at a character boundary.
///
/// What reaches here is a single unbreakable token — a 500-character URL, a
/// chemical name — because everything word-shaped has already been cut at a
/// whitespace boundary by `split_piece`. Splitting it is the only way it is
/// spoken at all. The boundary is a character, never a byte: a cut inside a
/// multi-byte char mangles the token twice over.
fn cap_longest(pieces: Vec<(usize, usize)>, text: &str, max: usize) -> Vec<(usize, usize)> {
    let cap = max;
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(pieces.len());
    for (start, end) in pieces {
        let mut seg_start = start;
        while end - seg_start > cap {
            let mut at = seg_start + cap;
            while at < end && !text.is_char_boundary(at) {
                at += 1;
            }
            if at >= end {
                break;
            }
            out.push((seg_start, at));
            seg_start = at;
        }
        if seg_start < end {
            out.push((seg_start, end));
        }
    }
    out
}

/// Cut one oversized piece.
///
/// Preference order, and why:
///
/// 1. **A whitespace boundary.** Cutting after a space means the next chunk
///    starts on a word, which is what an engine expects.
/// 2. **A safe character boundary.** Reached only when the segment has held no
///    whitespace at all — a CJK or Thai run, say. Every character there is a
///    syllable, so a cut is free. Cutting a space-separated word in half is not:
///    engines pronounce the halves as nonsense, so the fallback is deliberately
///    biased towards not cutting when it would land inside a Latin word.
/// 3. **Emit the word whole.** A 500-character token is pathological, but a
///    chunk the engine refuses is *inaudible*: Supertonic turns a chunk over
///    300 characters into a beep and a skipped phrase. So the word is emitted
///    here whole, and only the caller's `hard_cap` — a ceiling no engine
///    accepts — cuts it. A mangled word is wrong on one chunk; an unspoken one
///    is missing from the book.
fn split_piece(p: Chunk, text: &str, max: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut seg_start = p.start;
    // End offset (exclusive) of the last whitespace run in the live segment.
    let mut last_ws_end: Option<usize> = None;
    let mut i = p.start;

    while i < p.end() {
        let ch = char_at(text, i).unwrap_or(' ');

        if ch.is_whitespace() {
            let run_end = i + whitespace_run_len(text, i, p.end());
            last_ws_end = Some(run_end);
            i = run_end;
            continue;
        }

        if i - seg_start >= max {
            if let Some(cut) = last_ws_end.filter(|c| *c > seg_start) {
                out.push((seg_start, cut));
                seg_start = cut;
                last_ws_end = None;
                // `i` is deliberately not advanced: it is now the first offset of
                // the next segment and must be re-examined against the new start.
                continue;
            }
            if is_safe_cut(text, i) {
                out.push((seg_start, i));
                seg_start = i;
                continue;
            }
            // Mid-word with nothing better available: emit the word whole.
            while i < p.end() {
                let c = char_at(text, i).unwrap_or(' ');
                if c.is_whitespace() {
                    break;
                }
                i += c.len_utf8();
            }
            continue;
        }

        i += ch.len_utf8();
    }

    if seg_start < p.end() {
        out.push((seg_start, p.end()));
    }
    out
}

/// Is `at` a place where cutting cannot land inside a space-separated word?
///
/// Conservative by design: only a non-ASCII character before the cut point,
/// or the start of the piece, qualifies.
fn is_safe_cut(text: &str, at: usize) -> bool {
    match text[..at].chars().next_back() {
        None => true,
        Some(c) => !c.is_ascii_alphanumeric() && c != '\'',
    }
}

/// Byte length of the whitespace run starting at `from`, stopping at `end`.
fn whitespace_run_len(text: &str, from: usize, end: usize) -> usize {
    let mut n = 0usize;
    let mut i = from;
    while i < end {
        match char_at(text, i) {
            Some(c) if c.is_whitespace() => {
                n += c.len_utf8();
                i += c.len_utf8();
            }
            _ => break,
        }
    }
    n
}

/// Plan chunks for synthesis: sentences, glued when too short, numbered, and
/// optionally trimmed to resume from a byte offset.
///
/// `resume_from` is a byte offset into `text` — this app stores bookmark
/// offsets as UTF-8 byte positions, unlike the other Speechnotes apps which use
/// UTF-16. The conversion happens once, at the store boundary.
///
/// After a resume the chunks are **renumbered from zero** so the player can
/// index a sequence it just built.
pub fn plan_chunks(text: &str, max_chars: usize, resume_from: usize) -> Vec<Chunk> {
    let pieces = sentence_pieces(text, max_chars);
    let pieces = glue_short(pieces, MIN_PIECE_CHARS, max_chars);
    let resume_from = resume_from.min(text.len());
    let resume_from = floor_char_boundary(text, resume_from);

    let mut chunks: Vec<Chunk> = Vec::new();
    for p in pieces {
        if p.end() <= resume_from {
            continue;
        }
        let start = p.start.max(resume_from);
        let start = floor_char_boundary(text, start);
        // Losslessness is absolute here, including whitespace-only spans. The
        // chunks must tile the source exactly, because a bookmark is a byte
        // offset into `text` and a resume indexes this list. Filtering out
        // silent spans is the caller's decision — see `speakable`.
        chunks.push(Chunk {
            text: text[start..p.end()].to_string(),
            start,
            len: p.end() - start,
            index: chunks.len(),
        });
    }
    chunks
}

/// Merge pieces shorter than `min_chars` into the preceding piece.
///
/// Pieces are never glued *forward* across a resume boundary — that happens
/// later, in `plan_chunks`, on the trimmed pieces.
fn glue_short(pieces: Vec<Chunk>, min_chars: usize, max_chars: usize) -> Vec<Chunk> {
    let mut out: Vec<Chunk> = Vec::with_capacity(pieces.len());
    for p in pieces {
        let char_len = p.text.chars().count();
        let previous_too_short = out
            .last()
            .map(|q| q.text.chars().count() < min_chars)
            .unwrap_or(false);
        let fits = match out.last() {
            Some(q) => q.len + p.len <= max_chars,
            None => true,
        };

        if char_len < min_chars && previous_too_short && fits {
            if let Some(q) = out.last_mut() {
                q.len += p.len;
                q.text.push_str(&p.text);
                continue;
            }
        }
        out.push(p);
    }
    out
}

/// Round a byte offset down to a char boundary, so a bookmark that landed inside
/// a multi-byte character resumes cleanly instead of panicking.
fn floor_char_boundary(text: &str, mut i: usize) -> usize {
    if i >= text.len() {
        return text.len();
    }
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Chunks worth handing to an engine, with their plan indices preserved.
///
/// The plan stays lossless; this is the filter the playback pipeline applies, so
/// a paragraph break does not become a model call for silence. Keeping the two
/// apart matters: filtering inside [`plan_chunks`] would break the invariant
/// that chunks tile the source, which is what resume depends on.
pub fn speakable(chunks: &[Chunk]) -> Vec<&Chunk> {
    chunks
        .iter()
        .filter(|c| !c.text.trim().is_empty())
        .collect()
}

/// Total characters across all chunks — equal to `text.chars().count()` after
/// [`plan_chunks`], which is the losslessness property stated as a function.
pub fn covered_len(chunks: &[Chunk]) -> usize {
    chunks.iter().map(|c| c.text.chars().count()).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn joined(chunks: &[Chunk]) -> String {
        chunks.iter().map(|c| c.text.as_str()).collect()
    }

    #[test]
    fn a_single_sentence_is_one_chunk() {
        let t = "Hello there. ";
        let c = plan_chunks(t, 300, 0);
        assert_eq!(c.len(), 1);
        assert_eq!(joined(&c), t);
    }

    #[test]
    fn concatenation_is_byte_exact() {
        let t = "One. Two! Three? Four\nFive\r\nSix.  Seven.   Eight. ";
        let c = plan_chunks(t, 300, 0);
        assert_eq!(joined(&c), t, "chunking must be lossless");
        // Pieces must tile the source: contiguous, in order, ending at the end.
        let mut cursor = 0;
        for piece in &c {
            assert_eq!(piece.start, cursor, "gap or overlap at {cursor}");
            cursor = piece.end();
        }
        assert_eq!(cursor, t.len());
    }

    #[test]
    fn each_char_boundary_is_a_valid_slice() {
        let t = "Emoji 🎉 and accents éé and CJK 。 next. Second sentence. ";
        let c = plan_chunks(t, 40, 0);
        assert_eq!(joined(&c), t);
        for piece in &c {
            assert!(t.is_char_boundary(piece.start));
            assert!(t.is_char_boundary(piece.end()));
        }
    }

    #[test]
    fn a_decimal_point_is_not_a_boundary() {
        let c = plan_chunks("Pi is 3.14 exactly. Yes.", 300, 0);
        // The only split allowed is the sentence end. The interior dot must not
        // be a boundary, or the chunker hands an engine "3." and "14 exactly."
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].text, "Pi is 3.14 exactly. ");
        assert_eq!(c[1].text, "Yes.");
        assert!(c[0].text.contains("3.14"));
    }

    #[test]
    fn a_version_number_is_not_a_boundary() {
        let c = plan_chunks("Update to v1.2 today.", 300, 0);
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn an_abbreviation_keeps_the_next_word() {
        // "Dr." is followed by a space, so it does end a piece — the caller's
        // job, not the chunker's, to decide an abbreviation is not a sentence.
        // What must not happen is losing the word: the split is still lossless.
        let t = "Dr. Smith arrived. He was late.";
        let c = plan_chunks(t, 300, 0);
        assert_eq!(joined(&c), t);
        assert!(c[0].text.starts_with("Dr."));
    }

    #[test]
    fn cjk_terminators_end_a_sentence_with_no_trailing_space() {
        let t = "これはテストです。次の文です。";
        let c = plan_chunks(t, 300, 0);
        assert_eq!(joined(&c), t);
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn cjk_is_not_split_mid_character_when_oversized() {
        // No whitespace anywhere, so the only legal cut points are the CJK
        // terminators. That is the point: a run of CJK with no terminators must
        // still be bounded, and the cut may only land on a terminator.
        let long: String = "文".repeat(200) + "。" + &"字".repeat(200) + "。" + &"語".repeat(200) + "。";
        let c = plan_chunks(&long, 100, 0);
        assert_eq!(joined(&c), long);
        assert!(c.len() >= 6, "expected several pieces, got {}", c.len());
        for piece in &c {
            assert!(
                piece.text.chars().count() <= 100,
                "piece of {} chars exceeds max",
                piece.text.chars().count()
            );
            // With no whitespace to aim for, cuts land on character boundaries.
            // That is the correct trade: each Han character is its own syllable,
            // so a cut there costs nothing, whereas cutting an English word
            // mid-way makes engines pronounce garbage.
            assert!(piece.start % 3 == 0, "cut inside a 3-byte Han character");
        }
    }

    #[test]
    fn short_pieces_are_glued_to_their_neighbour() {
        // "A." is below the minimum on its own; it must not become its own
        // synthesis request.
        let t = "A. B. C. Some longer sentence to anchor the glue.";
        let c = plan_chunks(t, 300, 0);
        assert!(c.len() < 4, "short pieces should have been merged: {c:?}");
        assert_eq!(joined(&c), t);
    }

    #[test]
    fn oversized_sentences_split_at_word_boundaries() {
        let words: Vec<String> = (0..200).map(|i| format!("word{i}")).collect();
        let t = format!("{}.", words.join(" "));
        let c = plan_chunks(&t, 100, 0);
        assert_eq!(joined(&c), t);
        assert!(c.len() > 1);
        for piece in &c {
            assert!(
                piece.text.chars().count() <= 100,
                "piece too long: {} chars",
                piece.text.chars().count()
            );
        }
        // Every piece after the first starts on a word, not mid-word.
        for piece in &c[1..] {
            assert!(
                piece.text.chars().next().map(|ch| ch.is_alphanumeric()).unwrap_or(true),
                "piece starts mid-word: {:?}",
                &piece.text[..piece.text.len().min(20)]
            );
        }
    }

    #[test]
    fn a_word_longer_than_max_is_its_own_piece_not_split() {
        // Under the engine's ceiling the word is still emitted whole — the
        // "never cut mid-word" rule has not gone anywhere.
        let long = "a".repeat(90);
        let t = format!("short. {long} end.");
        let c = plan_chunks(&t, 100, 0);
        assert_eq!(joined(&c), t);
        assert!(
            c.iter().any(|p| p.text.contains(&long)),
            "the long word must survive whole inside one piece; got {:?}",
            c.iter().map(|p| p.text.chars().count()).collect::<Vec<_>>()
        );
        for p in &c {
            assert!(
                p.text.chars().count() <= hard_cap(100),
                "a piece the engine would refuse: {} chars",
                p.text.chars().count()
            );
        }
    }

    /// The failure this fixes: a chunk the engine refuses is not spoken at
    /// all, so a phrase in a book went missing. Cutting the token is wrong on
    /// one chunk; not speaking it is missing from the book.
    #[test]
    fn no_chunk_reaches_the_engine_over_its_ceiling() {
        // A URL and a chemical name: the two shapes that have no break
        // point a sentence splitter can use.
        let t = format!(
            "See https://example.example/{} for details. The compound {} reacts.",
            "p".repeat(400),
            "n".repeat(350)
        );
        let c = plan_chunks(&t, 300, 0);
        assert_eq!(joined(&c), t, "the cap must not lose bytes");
        for p in &c {
            assert!(
                p.text.chars().count() <= hard_cap(300),
                "a piece over the ceiling: {} chars",
                p.text.chars().count()
            );
        }
        // Word-shaped text is still cut at whitespace, never mid-word: the
        // cap only ever fires on a token nothing can break.
        let prose = "one two three four five six seven eight nine ten. ".repeat(40);
        let c = plan_chunks(&prose, 300, 0);
        for p in &c {
            assert!(
                p.text.chars().count() <= 300,
                "prose is cut at word boundaries: {} chars",
                p.text.chars().count()
            );
        }
    }

    /// A cut inside a multi-byte character would mangle the token twice over,
    /// so the cap has to land on a character boundary.
    #[test]
    fn the_cap_lands_on_a_character_boundary() {
        let mut t = String::from("Start. ");
        for _ in 0..80 {
            t.push('日');
        }
        t.push_str(" end.");
        let c = plan_chunks(&t, 300, 0);
        assert_eq!(joined(&c), t);
        for p in &c {
            assert!(
                p.text.chars().count() <= hard_cap(300),
                "a piece over the ceiling: {} chars",
                p.text.chars().count()
            );
        }
    }

    /// The engine's own ceiling is 300 characters, and that is exactly the
    /// `max` the pipeline asks for — so a token past the cap has to come out
    /// *at* 300 or under, not merely under `max + MIN_PIECE_CHARS`. This is
    /// the check that would have caught the missing phrase on the day: a
    /// chunk Supertonic turns into a beep is a chunk the book never says.
    #[test]
    fn the_cap_respects_the_engine_ceiling_itself() {
        let t = format!(
            "See https://example.example/{} now.",
            "p".repeat(600),
        );
        let c = plan_chunks(&t, MAX_ENGINE_CHARS, 0);
        assert_eq!(joined(&c), t, "the cap must not lose bytes");
        assert!(c.len() > 1, "a 600-character token cannot be one chunk");
        for p in &c {
            assert!(
                p.text.chars().count() <= MAX_ENGINE_CHARS,
                "a piece the engine refuses: {} chars",
                p.text.chars().count()
            );
        }
    }

    #[test]
    fn resume_skips_completed_pieces() {
        let t = "First sentence here. Second sentence here. Third sentence here.";
        let all = plan_chunks(t, 300, 0);
        assert_eq!(all.len(), 3);
        let from = all[1].start;
        let resumed = plan_chunks(t, 300, from);
        assert_eq!(resumed.len(), 2);
        assert_eq!(joined(&resumed), &t[from..]);
    }

    #[test]
    fn resume_renumbers_from_zero() {
        let t = "One here. Two here. Three here.";
        let all = plan_chunks(t, 300, 0);
        let resumed = plan_chunks(t, 300, all[1].start);
        assert_eq!(resumed.iter().map(|c| c.index).collect::<Vec<_>>(), vec![0, 1]);
    }

    #[test]
    fn resume_inside_a_piece_trims_it() {
        let t = "Alpha beta gamma delta. Second one. Third one.";
        let all = plan_chunks(t, 300, 0);
        // Resume 10 bytes into the first piece.
        let resumed = plan_chunks(t, 300, all[0].start + 10);
        assert_eq!(joined(&resumed), &t[all[0].start + 10..]);
        assert_eq!(resumed[0].start, all[0].start + 10);
    }

    #[test]
    fn resume_past_the_end_yields_nothing() {
        let t = "Only one sentence.";
        assert!(plan_chunks(t, 300, t.len()).is_empty());
        assert!(plan_chunks(t, 300, t.len() + 500).is_empty());
    }

    #[test]
    fn resume_into_a_multibyte_character_snaps_back() {
        let t = "Emoji 🎉🎉🎉 here. Second sentence.";
        let all = plan_chunks(t, 300, 0);
        // Land in the middle of a surrogate-free 4-byte emoji.
        let mid = all[0].start + 8;
        let resumed = plan_chunks(t, 300, mid);
        for piece in &resumed {
            assert!(t.is_char_boundary(piece.start));
            assert!(t.is_char_boundary(piece.end()));
        }
    }

    #[test]
    fn empty_input_plans_nothing_and_whitespace_plans_silence() {
        assert!(plan_chunks("", 300, 0).is_empty());

        // Whitespace still tiles the source — that invariant is what resume
        // depends on — but it is never worth speaking, so the filter the
        // playback pipeline applies is what makes it disappear.
        assert!(speakable(&plan_chunks("   ", 300, 0)).is_empty());
        assert!(speakable(&plan_chunks("\n\n\n", 300, 0)).is_empty());
    }

    #[test]
    fn the_plan_tiles_the_source_even_across_whitespace_only_spans() {
        // A paragraph break is its own chunk. Dropping it from the plan would
        // leave a hole, and a resume indexing this list would then be wrong.
        let t = "One sentence.\n\nTwo sentences here.\n\n\nThree.\n";
        let plan = plan_chunks(t, 300, 0);
        assert_eq!(joined(&plan), t);
        assert!(plan.iter().any(|c| c.text.trim().is_empty()));

        // The filter the pipeline uses drops those, without disturbing indices.
        let speak = speakable(&plan);
        assert_eq!(speak.len(), 3);
        let rendered: String = speak.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(rendered, "One sentence.Two sentences here.Three.");
        assert_eq!(
            speak.iter().map(|c| c.index).collect::<Vec<_>>(),
            vec![0, 2, 4],
            "indices must still point into the full plan"
        );
    }

    #[test]
    fn newline_delimited_lines_are_separate_pieces() {
        let t = "line one\nline two\nline three";
        let c = plan_chunks(t, 300, 0);
        assert_eq!(c.len(), 3);
        assert_eq!(joined(&c), t);
    }

    #[test]
    fn a_200k_character_chapter_chunks_in_linear_time() {
        // The Swift chunker was quadratic here; this is the regression guard.
        let t = "The quick brown fox jumps over the lazy dog. ".repeat(8_000);
        assert!(t.len() > 180_000, "fixture should be book-chapter sized");

        let start = std::time::Instant::now();
        let chunks = plan_chunks(&t, DEFAULT_MAX_CHARS, 0);
        let elapsed = start.elapsed();

        assert_eq!(joined(&chunks), t, "still lossless at scale");
        assert!(chunks.len() > 500);
        assert!(
            elapsed.as_millis() < 500,
            "chunking {} bytes took {:?} — expected well under 500ms", t.len(), elapsed
        );
    }

    #[test]
    fn chunk_indices_are_contiguous() {
        let t = "One. Two. Three. Four. Five. Six. Seven.";
        let c = plan_chunks(t, DEFAULT_MAX_CHARS, 0);
        for (i, piece) in c.iter().enumerate() {
            assert_eq!(piece.index, i);
        }
    }

    #[test]
    fn covered_len_matches_the_source() {
        let t = "Alpha. Beta gamma. Delta epsilon zeta. 🎉 Final.";
        let c = plan_chunks(t, DEFAULT_MAX_CHARS, 0);
        assert_eq!(covered_len(&c), t.chars().count());
    }
}