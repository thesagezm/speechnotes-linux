//! Text hygiene for the speech pipeline.
//!
//! The engines fail on exactly the bytes a paste or an import can bring in —
//! soft hyphens from a copied PDF line, zero-width joiners, control bytes —
//! and they read markup noise ("hash hash heading") as words. The sanitizer
//! is the boundary between the note the user sees and the text an engine
//! hears, and it has one absolute property: **it is byte-for-byte with the
//! source**. Removed markup becomes spaces at its own positions, so a byte
//! offset in the cleaned text is the same offset in the note — which is what
//! lets `plan_chunks` tile the cleaned text and the resulting bookmarks point
//! back into the raw one.
//!
//! The public items (`clean`, `clean_line`, `is_speakable`) are the call
//! surface sn-tts's plan module and the reader/player consume. Hard rules,
//! each pinned by a test: `clean` emits exactly one byte per input byte
//! (removed markup becomes spaces), fenced-code content never reaches a
//! chunk, and `...`/`!!!` runs are filtered as unspeakable.

/// One pass of invisible-character removal over a single line.
///
/// Zero-width spaces, bidi marks, variation selectors, soft hyphens, the BOM
/// and control characters are engine poison: an engine beeps or skips the
/// chunk that carries them. Each offending character is replaced with space
/// bytes at its own position, so the line keeps its length and every
/// non-invisible byte stays put. Tab (and newline) are content, not noise,
/// and survive untouched.
// The same-item push inside is the intentional byte-padding loop: one space
// per source byte keeps the 1:1 byte mapping.
#[allow(clippy::same_item_push)]
pub fn clean_line(line: &str) -> String {
    let mut out = Vec::with_capacity(line.len());
    for ch in line.chars() {
        if is_invisible(ch) {
            // One space per source BYTE keeps the 1:1 byte mapping.
            for _ in 0..ch.len_utf8() {
                out.push(b' ');
            }
        } else {
            let mut buf = [0u8; 4];
            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
        }
    }
    // Every byte written is either an original byte or an ASCII space, so
    // the buffer is valid UTF-8 whenever the input was.
    String::from_utf8(out).unwrap_or_default()
}

/// True for the characters [`clean_line`] strips.
fn is_invisible(ch: char) -> bool {
    matches!(ch,
        '\u{00AD}'                      // soft hyphen
        | '\u{180E}'                    // Mongolian vowel separator
        | '\u{200B}'..='\u{200F}'       // zero-width space/joiner + LRM/RLM
        | '\u{202A}'..='\u{202E}'       // bidi embedding controls
        | '\u{2060}'..='\u{2064}'       // word joiner + invisible operators
        | '\u{2066}'..='\u{2069}'       // bidi isolate controls
        | '\u{FEFF}'                    // BOM / zero-width no-break space
        | '\u{FE00}'..='\u{FE0F}'       // variation selectors
        | '\u{E0100}'..='\u{E01EF}'     // variation selectors supplement
    ) || (ch.is_control() && ch != '\t' && ch != '\n' && ch != '\r')
}

/// Markdown → speakable text, byte-for-byte with the source.
///
/// Block and inline markup is blanked (replaced with spaces), never removed:
/// fenced code blocks whole — the engines must not read program text —
/// heading, quote, and list markers, emphasis and inline-code ticks,
/// thematic breaks, table separator rows and pipes, and the plumbing of
/// links and images (the label or alt text stays, the URL goes). The result
/// feeds [`crate::chunker::plan_chunks`] directly; the speakable filter on
/// the plan drops the all-punctuation runs this leaves behind.
pub fn clean(text: &str) -> String {
    let mut out = text.as_bytes().to_vec();
    // The fence character while inside a fenced code block.
    let mut in_fence: Option<u8> = None;

    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        // The newline itself is never markup: blank only the content span so
        // the line structure the chunker relies on survives.
        let content_len = line.trim_end_matches(['\n', '\r']).len();
        let content = &text[start..start + content_len];

        // Fenced code: the fence lines and everything inside go blank.
        match in_fence {
            Some(fence) => {
                blank(&mut out, start, content_len);
                if is_closing_fence(content, fence) {
                    in_fence = None;
                }
                continue;
            }
            None => {
                if let Some(fence) = opening_fence(content) {
                    in_fence = Some(fence);
                    blank(&mut out, start, content_len);
                    continue;
                }
            }
        }

        let trimmed = content.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        // Whole-line constructs: the line IS markup.
        if is_thematic_break(trimmed) || is_table_separator(trimmed) {
            blank(&mut out, start, content_len);
            continue;
        }
        // Prefix constructs: the marker goes, the text stays.
        let indent = content.len() - trimmed.len();
        let prefix = match markup_prefix(trimmed) {
            Some(len) => len,
            None => 0,
        };
        if prefix > 0 {
            blank(&mut out, start + indent, prefix);
        }
        // Inline constructs over the remainder of the line.
        let body_start = start + indent + prefix;
        clean_inline(&mut out, body_start, &text[body_start..start + content_len]);
    }

    String::from_utf8(out).unwrap_or_default()
}

/// The length of the markup prefix of a non-blank, non-fence line, if it
/// starts with one: heading, quote, or list markers.
fn markup_prefix(trimmed: &str) -> Option<usize> {
    let b = trimmed.as_bytes();
    match b.first()? {
        b'#' => {
            let hashes = b.iter().take_while(|&&x| x == b'#').count();
            if !(1..=6).contains(&hashes) {
                return None; // seven hashes is text, not a heading
            }
            match b.get(hashes) {
                None | Some(b' ') | Some(b'\t') => {
                    Some(hashes + usize::from(b.get(hashes) == Some(&b' ')))
                }
                _ => None,
            }
        }
        b'>' => Some(1 + usize::from(b.get(1) == Some(&b' '))),
        b'-' | b'*' | b'+' if b.get(1) == Some(&b' ') => Some(2),
        _ => {
            if ordered_marker_end(trimmed).is_some() {
                let digits = b.iter().take_while(|x| x.is_ascii_digit()).count();
                Some(digits + 2) // "1. " / "12) "
            } else {
                None
            }
        }
    }
}

/// Blank the inline syntax of one line, in place, over the byte span that
/// holds it. Link labels and image alt text survive; URLs, brackets, and
/// emphasis markers become spaces.
fn clean_inline(out: &mut [u8], offset: usize, line: &str) {
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'*' | b'`' | b'~' | b'_' | b'^' | b'|' => {
                out[offset + i] = b' ';
                i += 1;
            }
            b'\\' => {
                // An escape is markup: the backslash goes, the escaped
                // character stays and reads literally.
                out[offset + i] = b' ';
                i += 2;
            }
            b'!' if bytes.get(i + 1) == Some(&b'[') => {
                i += link_span(out, offset, line, i + 1, true);
            }
            b'[' => {
                i += link_span(out, offset, line, i, false);
            }
            b'<' => {
                // `<scheme://…>` autolink: the brackets are markup, the URL
                // stays (lettered, so the plan's speakable filter — not the
                // byte count — decides its fate).
                if let Some(close) = bytes[i + 1..].iter().position(|&b| b == b'>') {
                    out[offset + i] = b' ';
                    out[offset + i + 1 + close] = b' ';
                    i += close + 2;
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
}

/// Blank the plumbing of `[label](target)` starting at `open` (the `[`),
/// keeping the bracketed text; a reference form `[label][key]` or a literal
/// bracket keeps its text and loses only the brackets. Returns how far the
/// caller may advance.
fn link_span(out: &mut [u8], offset: usize, line: &str, open: usize, image: bool) -> usize {
    let _ = image;
    let bytes = line.as_bytes();
    let Some(close_bracket_rel) = bytes[open..].iter().position(|&b| b == b']') else {
        return 1;
    };
    let close_bracket = open + close_bracket_rel;
    match bytes.get(close_bracket + 1) {
        Some(b'(') => {
            let Some(close_paren_rel) = bytes[close_bracket + 2..].iter().position(|&b| b == b')')
            else {
                return 1;
            };
            let close_paren = close_bracket + 2 + close_paren_rel;
            out[offset + open] = b' ';
            out[offset + close_bracket] = b' ';
            out[offset + close_bracket + 1] = b' ';
            out[offset + close_paren] = b' ';
            // The target between the parens goes too.
            for b in &mut out[offset + close_bracket + 2..offset + close_paren] {
                *b = b' ';
            }
            close_paren + 1 - open
        }
        _ => {
            out[offset + open] = b' ';
            out[offset + close_bracket] = b' ';
            close_bracket + 1 - open
        }
    }
}

fn blank(out: &mut [u8], at: usize, len: usize) {
    // The bound is computed first: indexing and `.min(out.len())` in one
    // expression borrows `out` mutably and immutably at once.
    let end = (at + len).min(out.len());
    for b in &mut out[at..end] {
        *b = b' ';
    }
}

/// ``` / ~~~ with at least three of one kind opens a fenced block.
fn opening_fence(line: &str) -> Option<u8> {
    let t = line.trim_start();
    let b = t.as_bytes();
    let fence = *b.first()?;
    if fence != b'`' && fence != b'~' {
        return None;
    }
    (b.iter().take_while(|&&x| x == fence).count() >= 3).then_some(fence)
}

/// A closing fence: only the fence character (and padding) on the line.
fn is_closing_fence(line: &str, fence: u8) -> bool {
    let t = line.trim();
    !t.is_empty() && t.bytes().all(|b| b == fence)
}

/// `---`, `***`, `___` — three or more of one kind, spaces allowed between.
fn is_thematic_break(line: &str) -> bool {
    let b = line.trim().as_bytes();
    let Some(&marker) = b.first() else { return false };
    if !matches!(marker, b'-' | b'*' | b'_') {
        return false;
    }
    b.iter().filter(|&&x| x == marker).count() >= 3
        && b.iter().all(|&x| x == marker || x == b' ' || x == b'\t')
}

/// A GFM table separator row: only `|`, `-`, `:` and spaces, with both chars
/// present so an em-dash prose line is not mistaken for one.
fn is_table_separator(line: &str) -> bool {
    let b = line.trim().as_bytes();
    b.contains(&b'|')
        && b.contains(&b'-')
        && b.iter().all(|&b| matches!(b, b'|' | b'-' | b':' | b' ' | b'\t'))
}

/// End offset of an ordered-list marker (`1. `, `12) `), if present.
fn ordered_marker_end(line: &str) -> Option<usize> {
    let digits = line.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    match (line.as_bytes().get(digits), line.as_bytes().get(digits + 1)) {
        (Some(b'.') | Some(b')'), Some(b' ')) => Some(digits + 2),
        _ => None,
    }
}

/// Would an engine say anything here?
///
/// The gate before a session starts (a note) and before a page is spoken (a
/// book chapter): text with no letters — punctuation runs, rules, blanked
/// code, a bare URL — is silence at best and a model error at worst. The
/// reader's call site treats `false` as "this page has no text" and looks
/// ahead, so the bar is deliberately conservative.
///
/// Pinned by the unspeakable-punctuation plan test (`"Before. ... !!! —
/// After."` plans as exactly two chunks) and the empty-plan test on
/// `"---\n***\n..."`.
pub fn is_speakable(text: &str) -> bool {
    // True when at least one non-URL word carries letters.
    text
        .split_whitespace()
        .filter(|word| !looks_like_url(word))
        .any(|word| word.chars().any(char::is_alphabetic))
}

/// True for `scheme://…`, `www.…` and `user@host` shapes.
fn looks_like_url(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    lower.contains("://")
        || lower.starts_with("www.")
        || (lower.contains('@') && lower.contains('.'))
}

/// Returns the cleaned text alongside the map back to original byte offsets, so
/// a caller that needs to report "we spoke this much" can. The map holds, for
/// each byte of output, the byte of input it came from; it is `len + 1` long so
/// the end position is always addressable.
pub fn normalize_whitespace(text: &str) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(text.len());
    let mut map: Vec<usize> = Vec::with_capacity(text.len() + 1);
    // The first byte of the whitespace run a pending space stands in for.
    let mut pending_from: Option<usize> = None;

    for (i, ch) in text.char_indices() {
        if ch.is_whitespace() {
            if !out.is_empty() && pending_from.is_none() {
                pending_from = Some(i);
            }
            continue;
        }
        if let Some(run_start) = pending_from.take() {
            out.push(' ');
            // The synthetic space maps to the whitespace run it replaced, so
            // mapping a position back lands on a real space in the source
            // rather than on the preceding character.
            map.push(run_start);
        }
        out.push(ch);
        map.push(i);
    }

    map.push(text.len());
    (out, map)
}

/// A byte offset in normalised text, and where it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OffsetSpan {
    /// Offset in the normalised string.
    pub normalized: usize,
    /// Offset in the original string.
    pub original: usize,
}

/// Map a byte offset in normalised text back to the original text.
///
/// Offsets past the end clamp to the end. An offset that lands inside a
/// collapsed whitespace run snaps to the run's first byte, which is the closest
/// stable answer: resuming from the middle of a space run has no meaning.
pub fn original_offset(normalized: &str, map: &[usize], offset: usize) -> usize {
    if offset == 0 {
        return map.first().copied().unwrap_or(0);
    }
    if offset >= normalized.len() {
        return map.last().copied().unwrap_or(normalized.len());
    }
    map.get(offset.min(map.len().saturating_sub(1)))
        .copied()
        .unwrap_or(0)
}

/// Map an original byte offset forward into normalised text.
///
/// Used when a bookmark is recorded against the source and resume has to find
/// the same place in the sanitised text the engine will actually see.
///
/// A binary search rather than a scan: this runs once per resume, but it also
/// runs once per highlighted sentence during read-along, and a linear scan over
/// a 200k-character chapter on every sentence would be the kind of quadratic
/// behaviour this rewrite exists to remove.
pub fn normalized_offset(normalized: &str, map: &[usize], original: usize) -> usize {
    match map.binary_search_by(|probe| probe.cmp(&original)) {
        Ok(i) => i,
        // No exact hit: the offset fell inside a collapsed whitespace run or at
        // a boundary. The first mapped position at or after it is the closest
        // honest answer — snapping back would land on text before the bookmark.
        Err(i) => i.min(normalized.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_is_byte_for_byte_with_the_source() {
        // The one absolute property: offsets never move.
        let text = "# Heading\n\nA sentence with **bold** and a [link](http://x.example).\n\n```rust\nlet code = 1;\n```\n\nTail sentence.";
        assert_eq!(clean(text).len(), text.len());
    }

    #[test]
    fn clean_removes_a_heading_marker() {
        let cleaned = clean("# Title here");
        assert!(cleaned.contains("Title here"));
        assert!(!cleaned.contains('#'));
    }

    #[test]
    fn fenced_code_never_reaches_a_chunk() {
        let cleaned = clean("```rust\nlet code = 1;\n```\nTail sentence.");
        assert!(!cleaned.contains("let code"));
        assert!(cleaned.contains("Tail sentence"));
    }

    #[test]
    fn link_text_stays_but_the_url_goes() {
        let text = "see [the docs](http://x.example) now";
        let cleaned = clean(text);
        assert!(cleaned.contains("the docs"));
        assert!(!cleaned.contains("http://x.example"));
        assert_eq!(cleaned.len(), text.len());
    }

    #[test]
    fn clean_line_replaces_invisible_characters_in_place() {
        let line = "a\u{200B}b\u{00AD}c";
        let cleaned = clean_line(line);
        assert_eq!(cleaned.len(), line.len());
        // One space per source byte: ZWSP is 3 bytes, soft hyphen 2.
        assert_eq!(cleaned, "a   b  c");
        assert_eq!(clean_line("tab\tstays"), "tab\tstays");
    }

    #[test]
    fn punctuation_runs_are_not_speakable() {
        assert!(!is_speakable("..."));
        assert!(!is_speakable("!!! —"));
        assert!(!is_speakable("---"));
        assert!(!is_speakable(""));
    }

    #[test]
    fn urls_alone_are_not_speakable() {
        assert!(!is_speakable("http://x.example"));
        assert!(!is_speakable("https://y.example.org/page"));
        assert!(is_speakable("see the docs at https://y.example.org/page now"));
    }

    #[test]
    fn ordinary_text_is_speakable() {
        assert!(is_speakable("Before."));
        assert!(is_speakable("— After."));
        assert!(is_speakable("これはテストです。"));
    }
}
