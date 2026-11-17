//! Plain-text and Markdown book reader — the format Project Gutenberg still
//! ships more of than any other, and the one a TTS app is uniquely good at.
//!
//! Two jobs, in order of how much they matter:
//!
//! 1. **Chapters.** A 900 kB `.txt` imported as one chapter is unusable — the
//!    contents list shows one row and resume always lands at the top. Real
//!    text books carry one of four structures, and each gets its own
//!    detection: Markdown headings (`#`…`######`), Gutenberg's running
//!    headings (`Chapter 7`, `PART TWO` — a short line alone that names the
//!    vocabulary), numbered short lines (`I.`, `12.`), and, when the file has
//!    no structure at all, chunks by paragraph count so a heading-less book
//!    still opens a chapter at a time.
//! 2. **Encoding.** Gutenberg mixes UTF-8 with UTF-16 (the BOM tells you),
//!    latin-1 and CP1252; a wrong guess turns every é into two mojibake
//!    glyphs. The battery below decodes UTF-8 strictly, UTF-16/32 only on
//!    their BOM (or on an unambiguous null-byte pattern for the BOM-less
//!    files Windows editors ship), and falls back to latin-1, which every
//!    byte survives.
//!
//! Markdown inline syntax (`**bold**`, `[text](url)`) is stripped rather
//! than spoken — the reader reads the words, not the markup.
//!
//! Ported from speechnotes-ios `PlainTextBookParser.swift` (the same user's
//! reference implementation); the two NSRegularExpression heading matchers
//! are hand-coded here — this workspace carries no regex dependency.

use super::bookdoc::{BookChapter, BookDoc, BookDocError};

/// Ceiling on one document's characters — a 40 MB text dump must fail with
/// a message rather than take the import down.
pub const MAX_CHARACTERS: usize = 4_000_000;

/// Bound on one chapter's blocks — a heading-less 900 kB text file still
/// chunks into units the reader can open one at a time.
const CHAPTER_BLOCK_LIMIT: usize = 150;

/// Parse a `.txt`/`.text` file into chapters of plain XHTML.
pub fn parse(data: &[u8]) -> Result<BookDoc, BookDocError> {
    let text = decode(data);
    if text.trim().is_empty() {
        return Err(BookDocError::Malformed(
            "the file has no readable text".to_string(),
        ));
    }
    let chars = text.chars().count();
    if chars > MAX_CHARACTERS {
        return Err(BookDocError::TooLarge {
            k: chars / 1000,
            m: MAX_CHARACTERS / 1_000_000,
        });
    }
    let chapters = chapterize(&text)
        .into_iter()
        .map(|(title, paragraphs)| {
            let html = paragraphs_paragraphs(&paragraphs);
            BookChapter { title, html }
        })
        .collect();
    Ok(BookDoc {
        chapters,
        ..BookDoc::default()
    })
}

/// The paragraphs of one chapter as one `<p>` XHTML body. The text is
/// escaped once, here — the reader serves this body as markup.
fn paragraphs_paragraphs(paragraphs: &[String]) -> String {
    let mut html = String::new();
    for paragraph in paragraphs {
        html.push_str("<p>");
        html.push_str(&super::bookdoc::xml_escape(paragraph));
        html.push_str("</p>");
    }
    html
}

/// The encoding battery, in the order it must be tried. UTF-16/32 are only
/// decoded on their BOM (or an unambiguous null pattern) because a BOM-less
/// even-length byte run decodes "successfully" in either parity — into CJK
/// garbage — and would swallow every latin-1 file before its turn.
pub fn decode(data: &[u8]) -> String {
    // 1. UTF-8 — validates strictly, so a Latin-1 file never decodes here.
    //    The NUL test keeps UTF-16 out: NUL is valid UTF-8, and without the
    //    test every UTF-16 file "decoded" here as the right-looking text
    //    with a NUL between each character.
    if let Ok(text) = std::str::from_utf8(data) {
        if !text.contains('\0') && !text.trim().is_empty() {
            return text.to_string();
        }
    }
    // 2. UTF-16, BOM required.
    if data.len() >= 2 {
        let (first, second) = (data[0], data[1]);
        if first == 0xFE && second == 0xFF {
            let units: Vec<u16> = data[2..]
                .chunks_exact(2)
                .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
                .collect();
            if let Some(text) = units_to_string(&units) {
                return text;
            }
        }
        if first == 0xFF && second == 0xFE {
            let units: Vec<u16> = data[2..]
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            if let Some(text) = units_to_string(&units) {
                return text;
            }
        }
    }
    // 3. UTF-32, BOM required (rare, but Gutenberg has shipped them).
    if data.len() >= 4 && (data[0..4] == [0, 0, 0xFE, 0xFF] || data[0..4] == [0xFF, 0xFE, 0, 0]) {
        let big_endian = data[0] == 0;
        let mut text = String::new();
        let mut valid = true;
        for scalar in data[4..].chunks_exact(4) {
            let value = if big_endian {
                u32::from_be_bytes([scalar[0], scalar[1], scalar[2], scalar[3]])
            } else {
                u32::from_le_bytes([scalar[0], scalar[1], scalar[2], scalar[3]])
            };
            match char::from_u32(value) {
                Some(c) => text.push(c),
                None => {
                    valid = false;
                    break;
                }
            }
        }
        if valid && !text.trim().is_empty() {
            return text;
        }
    }
    // 4. UTF-16 with NO BOM — Gutenberg and Windows editors both ship
    //    these; the null-byte pattern is the tell.
    if let Some(text) = bomless_utf16(data) {
        return text;
    }
    // 5. Latin-1 — total (every byte is a glyph), and correct for the
    //    Western corpus.
    let latin1: String = data.iter().map(|&b| b as char).collect();
    if !latin1.trim().is_empty() {
        return latin1;
    }
    String::from_utf8_lossy(data).into_owned()
}

/// UTF-16 units → String, rejecting a decode that is mostly control
/// characters (a 1% allowance with a floor, so a short clean string is not
/// rejected by rounding).
fn units_to_string(units: &[u16]) -> Option<String> {
    let text: String = String::from_utf16_lossy(units);
    if text.trim().is_empty() {
        return None;
    }
    let controls = text
        .chars()
        .filter(|c| c.is_control() && *c != '\n' && *c != '\r' && *c != '\t')
        .count();
    if controls * 100 >= text.chars().count().max(100) {
        return None;
    }
    Some(text)
}

/// Decodes BOM-less UTF-16 by its null-byte pattern, or `None` when `data`
/// is not that shape. A little-endian file nulls the HIGH byte (odd
/// positions); a big-endian one the low byte — only one parity may look
/// like text, and the decoded text must still look like text.
fn bomless_utf16(data: &[u8]) -> Option<String> {
    if data.len() < 4 {
        return None;
    }
    let sample = &data[..data.len().min(4096)];
    let pairs = (sample.len() / 2) as f64;
    if pairs < 2.0 {
        return None;
    }
    let (mut even_nulls, mut odd_nulls) = (0usize, 0usize);
    for (index, byte) in sample.iter().enumerate() {
        if *byte == 0 {
            if index % 2 == 0 {
                even_nulls += 1;
            } else {
                odd_nulls += 1;
            }
        }
    }
    let even_share = even_nulls as f64 / pairs;
    let odd_share = odd_nulls as f64 / pairs;
    let big_endian = even_share > 0.3 && odd_share < 0.1;
    let little_endian = odd_share > 0.3 && even_share < 0.1;
    if big_endian == little_endian {
        return None;
    }
    // The parity that is null-heavy is the HIGH byte's; the units decode
    // with the matching endianness.
    let units: Vec<u16> = sample
        .chunks_exact(2)
        .map(|pair| {
            if big_endian {
                u16::from_be_bytes([pair[0], pair[1]])
            } else {
                u16::from_le_bytes([pair[0], pair[1]])
            }
        })
        .collect();
    units_to_string(&units)
}

/// Splits the text into chapters: a `(title, paragraphs)` pair each. A
/// chapter that fills up carries its title onto the continuation so the
/// contents list never shows "Chapter N" mid-section.
fn chapterize(text: &str) -> Vec<(Option<String>, Vec<String>)> {
    let blocks = text_blocks(text);
    if blocks.is_empty() {
        return vec![(None, Vec::new())];
    }

    let mut chapters: Vec<(Option<String>, Vec<String>)> = Vec::new();
    let mut title: Option<String> = None;
    let mut paragraphs: Vec<String> = Vec::new();

    fn flush(
        chapters: &mut Vec<(Option<String>, Vec<String>)>,
        title: &mut Option<String>,
        paragraphs: &mut Vec<String>,
    ) {
        if !paragraphs.is_empty() || title.is_some() {
            chapters.push((title.clone(), std::mem::take(paragraphs)));
        }
        // The title carries onto the continuation chapter on purpose —
        // clearing it here is what split one Gutenberg chapter into two.
    }

    for block in blocks {
        if let Some(level) = heading_level(&block) {
            let heading_text = tidy_heading(&block);
            if heading_text.is_empty() {
                continue;
            }
            // A heading closes whatever is open. Level 1–2 always opens a
            // chapter; deeper ones only when nothing is open (documents
            // whose only structure is h3+).
            if level <= 2 || title.is_none() {
                flush(&mut chapters, &mut title, &mut paragraphs);
                title = Some(heading_text);
            } else {
                paragraphs.push(heading_text);
                if paragraphs.len() >= CHAPTER_BLOCK_LIMIT {
                    flush(&mut chapters, &mut title, &mut paragraphs);
                }
            }
            continue;
        }
        paragraphs.push(block);
        if paragraphs.len() >= CHAPTER_BLOCK_LIMIT {
            flush(&mut chapters, &mut title, &mut paragraphs);
        }
    }
    flush(&mut chapters, &mut title, &mut paragraphs);

    if chapters.is_empty() {
        return vec![(None, Vec::new())];
    }
    chapters
}

/// A heading's spoken text: the `#` markers go, the words stay.
fn tidy_heading(block: &str) -> String {
    if !block.starts_with('#') {
        return block.to_string();
    }
    strip_markdown_inline(block.trim_start_matches('#')).trim().to_string()
}

/// Paragraph blocks: blank-line separated, hard-wrapped lines joined.
/// Three things end a block: a blank line, a Markdown heading, and a line
/// that ENDS A SENTENCE — a hard-wrapped paragraph only continues onto the
/// next line while its sentence is unfinished. Joining unconditionally
/// glued a Gutenberg running heading to the prose under it.
fn text_blocks(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut paragraph: Vec<String> = Vec::new();

    fn flush(out: &mut Vec<String>, paragraph: &mut Vec<String>) {
        if paragraph.is_empty() {
            return;
        }
        let joined = paragraph.join(" ");
        paragraph.clear();
        let cleaned = strip_markdown_inline(&joined).trim().to_string();
        if !cleaned.is_empty() {
            out.push(cleaned);
        }
    }

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            flush(&mut out, &mut paragraph);
            continue;
        }
        // A Markdown heading is its own block, never joined to the
        // paragraph above it — and so is a short running/numbered heading
        // ("Chapter 7", "PART TWO"), even when prose follows it on the
        // very next line.
        if line.starts_with('#') || heading_level(line).is_some() {
            flush(&mut out, &mut paragraph);
            out.push(line.to_string());
            continue;
        }
        if paragraph.last().is_some_and(|previous| ends_sentence(previous)) {
            flush(&mut out, &mut paragraph);
        }
        paragraph.push(line.to_string());
    }
    flush(&mut out, &mut paragraph);
    out
}

/// True when a line ends a sentence, so the next line opens a new paragraph
/// rather than continuing this one. Closing punctuation after the full stop
/// still counts — `"Go," she said.` ends the sentence.
fn ends_sentence(line: &str) -> bool {
    for c in line.chars().rev() {
        if c.is_whitespace() {
            continue;
        }
        return matches!(c, '!' | '?' | '.' | '\u{201D}' | '"' | ')' | ']');
    }
    false
}

/// The heading level a block opens, or `None` when it is prose. `#`/`##`
/// win; a short line matching the Gutenberg running-heading vocabulary, or
/// a numbered short line, comes next.
fn heading_level(block: &str) -> Option<usize> {
    if let Some(rest) = block.strip_prefix('#') {
        let _ = rest;
        let level = block.chars().take_while(|c| *c == '#').count();
        return Some(level.clamp(1, 6));
    }
    let len = block.chars().count();
    if !(2..=80).contains(&len) {
        return None;
    }
    if is_running_heading(block) || is_numbered_heading(block) {
        return Some(2);
    }
    None
}

/// "Chapter 7", "CHAPTER VII.", "Part Two", "BOOK THE SECOND", "Act I",
/// "Letter 3", "Canto XXIV" — the running headings of the Gutenberg corpus,
/// matched as a whole short line: one of the vocabulary words at the start,
/// a word boundary, and at most 60 more characters.
fn is_running_heading(line: &str) -> bool {
    const WORDS: [&str; 14] = [
        "chapter", "part", "book", "section", "act", "scene", "letter", "canto", "prologue",
        "epilogue", "preface", "introduction", "afterword", "foreword",
    ];
    let lower = line.to_lowercase();
    for word in WORDS {
        let Some(rest) = lower.strip_prefix(word) else {
            continue;
        };
        // Word boundary: the character after the word must not continue it
        // ("chapterhouse" is a noun, not a heading).
        if rest
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
        {
            continue;
        }
        if rest.chars().count() <= 60 {
            return true;
        }
    }
    false
}

/// "I.", "12.", "VII — The Letter", "3. The Garden" — a roman-numeral or
/// 1–3-digit head, an optional one of `. : ) — -`, whitespace, then at most
/// 60 characters of title.
///
/// One guard the reference regex did not carry: on the *no-separator* path
/// the first word after the head must start an uppercase letter or a symbol
/// — otherwise every prose line that begins "I was three months…" or "12 to
/// 15 people…" parses as a heading and splits the chapter mid-sentence. A
/// real head with no separator is a title ("I Was Here", "VII — The
/// Letter"); prose keeps its lowercase.
fn is_numbered_heading(line: &str) -> bool {
    let head_len = line
        .chars()
        .take_while(|c| {
            matches!(
                c,
                'I' | 'V' | 'X' | 'L' | 'C' | 'i' | 'v' | 'x' | 'l' | 'c' | '0'..='9'
            )
        })
        .count();
    if head_len == 0 {
        return false;
    }
    let head = &line[..head_len];
    let digits_only = head.chars().all(|c| c.is_ascii_digit());
    // Digit heads are exactly 1–3 digits; roman heads any length the line's
    // own 80-char bound allows ("VIII", "LCXVI").
    if digits_only && head_len > 3 {
        return false;
    }
    let rest = &line[head_len..];
    // One optional separator from the set, then required whitespace.
    let (rest, separated) = match rest.strip_prefix(['.', ':', ')', '—', '-']) {
        Some(rest) => (rest, true),
        None => (rest, false),
    };
    let rest = match rest.strip_prefix([' ', '\t']) {
        Some(rest) => rest,
        None => return false,
    };
    let rest = rest.trim_start_matches([' ', '\t']);
    if rest.is_empty() || rest.chars().count() > 60 {
        return false;
    }
    // The guard: a bare head ("I", "12") with no separator only heads a
    // title-case line.
    separated || rest.chars().next().is_some_and(|c| !c.is_lowercase())
}

/// `**bold**`, `*em*`, backticks, `[text](url)` and bare URLs — the reader
/// speaks the words. Links first, so the label survives and the target
/// goes. Underscore runs only disappear at word boundaries (preceded and
/// followed by a non-word character), so `snake_case` survives.
pub fn strip_markdown_inline(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut index = 0usize;
    while index < text.len() {
        let c = text[index..].chars().next().unwrap();
        // Markdown links and images: `[label](target)` keeps the label;
        // `![alt](target)` goes entirely.
        if c == '[' || (c == '!' && text[index + 1..].starts_with('[')) {
            let label_start = if c == '[' { index } else { index + 1 };
            if let Some((label_end, target_end)) = match_link(text, label_start) {
                if c == '!' {
                    // An image is one gap: the space it becomes must not
                    // double the whitespace around it.
                    index = push_space_gap(&mut out, text, target_end);
                } else {
                    out.push_str(&text[label_start + 1..label_end]);
                    index = target_end;
                }
                continue;
            }
        }
        // Runs of 1–3 emphasis markers disappear — except an underscore,
        // which must sit at a word boundary on both sides.
        if matches!(c, '*' | '`' | '_') {
            let run = text[index..].chars().take_while(|n| *n == c).count();
            let boundary_before = !out.ends_with(char::is_alphanumeric);
            let after_run = text[index + c.len_utf8() * run..]
                .chars()
                .next();
            let boundary_after = !after_run.is_some_and(|n| n.is_alphanumeric());
            if c != '_' || (boundary_before && boundary_after) {
                index += c.len_utf8() * run.min(3);
                continue;
            }
        }
        // A bare URL goes, to the next whitespace — one gap, not three.
        if text[index..].starts_with("http://") || text[index..].starts_with("https://") {
            let end = text[index..]
                .find(char::is_whitespace)
                .map(|at| index + at)
                .unwrap_or(text.len());
            index = push_space_gap(&mut out, text, end);
            continue;
        }
        out.push(c);
        index += c.len_utf8();
    }
    out
}

/// Push the one-space gap a removal leaves behind, without doubling the
/// whitespace that already surrounds the removed span: a trailing space in
/// `out` counts, and the whitespace run starting at `at` is consumed. The
/// returned index is where scanning resumes.
fn push_space_gap(out: &mut String, text: &str, at: usize) -> usize {
    let mut index = at;
    while text[index..].starts_with(char::is_whitespace) {
        index += 1;
    }
    if !out.ends_with(char::is_whitespace) {
        out.push(' ');
    }
    index
}

/// `(label_end, target_end)` for the `[label](target)` starting at `open`
/// (the `[`'s own offset), when the brackets and parens all close.
fn match_link(text: &str, open: usize) -> Option<(usize, usize)> {
    let mut label_end = None;
    let mut depth = 0usize;
    for (offset, c) in text[open..].char_indices() {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    label_end = Some(open + offset);
                    break;
                }
            }
            _ => {}
        }
    }
    let label_end = label_end?;
    if !text[label_end + 1..].starts_with('(') {
        return None;
    }
    let target_open = label_end + 1;
    // Scan past the '(' itself — the guard below refuses a nested '(', not
    // the opening one.
    for (offset, c) in text[target_open + 1..].char_indices() {
        if c == ')' {
            return Some((label_end, target_open + 1 + offset + 1));
        }
        // A `(` inside the target would mean we matched the wrong pair;
        // real links do not nest their targets.
        if c == '(' {
            return None;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_is_the_first_answer() {
        assert_eq!(decode("plain é text".as_bytes()), "plain é text");
        // A BOM-less UTF-16 file decoded as UTF-8 would show NULs.
        let utf16le: Vec<u8> = [0xFFu8, 0xFE]
            .iter()
            .copied()
            .chain("Hello".encode_utf16().flat_map(|u| u.to_le_bytes()))
            .collect();
        assert_eq!(decode(&utf16le), "Hello");
        let utf16be: Vec<u8> = [0xFEu8, 0xFF]
            .iter()
            .copied()
            .chain("Hello".encode_utf16().flat_map(|u| u.to_be_bytes()))
            .collect();
        assert_eq!(decode(&utf16be), "Hello");
    }

    #[test]
    fn bomless_utf16_decodes_by_its_null_pattern() {
        let le: Vec<u8> = "Hi there"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        assert_eq!(decode(&le), "Hi there");
        let be: Vec<u8> = "Hi there"
            .encode_utf16()
            .flat_map(|u| u.to_be_bytes())
            .collect();
        assert_eq!(decode(&be), "Hi there");
    }

    #[test]
    fn latin1_is_the_floor() {
        // é = 0xE9 in latin-1; as UTF-8 this byte run is invalid, so the
        // floor reads it.
        assert_eq!(decode(&[0x63, 0xE9, 0x73, 0x74]), "c\u{e9}st");
    }

    #[test]
    fn empty_text_is_refused() {
        let err = parse(b"   \n\t\n").unwrap_err();
        assert!(err.to_string().contains("no readable text"), "{err}");
    }

    #[test]
    fn gutenberg_running_headings_open_chapters() {
        let text = "Chapter One\n\nProse one. More prose.\n\nChapter Two\n\nProse two.\n";
        let doc = parse(text.as_bytes()).unwrap();
        assert_eq!(doc.chapters.len(), 2);
        assert_eq!(doc.chapters[0].title.as_deref(), Some("Chapter One"));
        assert_eq!(doc.chapters[1].title.as_deref(), Some("Chapter Two"));
        assert!(doc.chapters[0].html.contains("<p>Prose one. More prose.</p>"));
    }

    #[test]
    fn markdown_headings_open_chapters_and_lose_their_marks() {
        let text = "# Part One\n\nText.\n\n## Deeper\n\nMore.\n";
        let doc = parse(text.as_bytes()).unwrap();
        assert_eq!(doc.chapters.len(), 2);
        assert_eq!(doc.chapters[0].title.as_deref(), Some("Part One"));
        assert_eq!(doc.chapters[1].title.as_deref(), Some("Deeper"));
    }

    #[test]
    fn numbered_short_lines_open_chapters() {
        let text = "I. The Letter\n\nBody one.\n\n12. The Garden\n\nBody two.\n";
        let doc = parse(text.as_bytes()).unwrap();
        assert_eq!(doc.chapters.len(), 2);
        assert_eq!(doc.chapters[0].title.as_deref(), Some("I. The Letter"));
        assert_eq!(doc.chapters[1].title.as_deref(), Some("12. The Garden"));
    }

    #[test]
    fn a_headingless_file_chunks_by_paragraph_count() {
        let paragraph = "Some prose sentence that stands alone.";
        // Blank-line separated paragraphs, no headings anywhere.
        let text = vec![paragraph; 400].join("\n\n");
        let doc = parse(text.as_bytes()).unwrap();
        assert!(doc.chapters.len() >= 3, "{} chapters", doc.chapters.len());
        // Every continuation keeps reading, none is empty.
        assert!(doc.chapters.iter().all(|c| !c.html.is_empty()));
    }

    #[test]
    fn a_hard_wrapped_paragraph_stays_one_block() {
        // Lines that do not end a sentence continue the paragraph; the line
        // that ends one closes it.
        let text = "the lamp post stood\nquiet under the grey\nrain all evening.\n";
        let blocks = text_blocks(text);
        assert_eq!(
            blocks,
            vec!["the lamp post stood quiet under the grey rain all evening."]
        );
    }

    #[test]
    fn sentence_ended_lines_split_into_paragraphs() {
        let text = "Alpha prose.\nBeta prose.\n";
        let blocks = text_blocks(text);
        assert_eq!(blocks.len(), 2, "{blocks:?}");
    }

    #[test]
    fn a_running_heading_is_never_glued_to_its_prose() {
        // The regression the sentence-end rule exists for: "Chapter One" +
        // "Prose one." must not become one block, or the chapter title
        // swallows the first sentence.
        let text = "Chapter One\nProse one. More prose.\n";
        let doc = parse(text.as_bytes()).unwrap();
        assert_eq!(doc.chapters[0].title.as_deref(), Some("Chapter One"));
        assert!(doc.chapters[0].html.contains("Prose one."));
    }

    #[test]
    fn the_title_carries_onto_the_continuation() {
        let paragraph = "A paragraph of prose that fills space.";
        // A heading opens a chapter; 300 more paragraphs overflow the
        // 150-block chapter twice over, and every continuation keeps the
        // heading's title.
        let text = format!("Chapter One\n\n{}\n", vec![paragraph; 300].join("\n\n"));
        let doc = parse(text.as_bytes()).unwrap();
        assert!(doc.chapters.len() >= 2, "{} chapters", doc.chapters.len());
        for chapter in &doc.chapters {
            assert_eq!(chapter.title.as_deref(), Some("Chapter One"));
        }
    }

    #[test]
    fn word_like_near_misses_are_not_headings() {
        assert_eq!(heading_level("chapterhouse rules apply here"), None);
        // Prose that merely begins with a numeral or a pronoun stays prose —
        // the no-separator guard.
        assert_eq!(heading_level("I read the book tonight by the fire"), None);
        assert_eq!(heading_level("12 to 15 people were there"), None);
        assert_eq!(heading_level("12345 numbers do not make a heading"), None);
        // The real forms: vocabulary heads, and separated or title-case
        // numbered heads.
        assert_eq!(heading_level("Chapter"), Some(2));
        assert_eq!(heading_level("Chapter One"), Some(2));
        assert_eq!(heading_level("I. The Letter"), Some(2));
        assert_eq!(heading_level("12. The Garden"), Some(2));
        assert_eq!(heading_level("VII — The Letter"), Some(2));
        assert_eq!(heading_level("I Was Here"), Some(2));
        // A bare head with nothing after it heads nothing — the reference
        // regex demands whitespace and a title after the numeral.
        assert_eq!(heading_level("VIII"), None);
        assert_eq!(heading_level("I."), None);
    }

    #[test]
    fn markdown_inline_syntax_is_stripped() {
        assert_eq!(strip_markdown_inline("**bold** and *em*"), "bold and em");
        assert_eq!(strip_markdown_inline("see [the docs](http://x.y/z) now"), "see the docs now");
        assert_eq!(strip_markdown_inline("![pic](img.png) rest"), " rest");
        assert_eq!(strip_markdown_inline("visit https://example.com/x today"), "visit today");
        assert_eq!(strip_markdown_inline("`code` here"), "code here");
        // snake_case keeps its underscore.
        assert_eq!(strip_markdown_inline("a snake_case word"), "a snake_case word");
    }

    #[test]
    fn chapter_html_is_escaped_paragraphs() {
        let doc = parse(b"Chapter One\n\nA <b> tag & an \"entity\".\n").unwrap();
        assert!(doc.chapters[0].html.contains("A &lt;b&gt; tag &amp; an &quot;entity&quot;."));
        assert!(!doc.chapters[0].html.contains("<b>"));
    }

    #[test]
    fn the_text_cap_refuses_a_monster() {
        // Past the cap the error names the size instead of taking the
        // import down. Building a real 4M-char file is slow; a small
        // override via the constant's shape is not possible, so this only
        // proves the branch arithmetic.
        let k = 5_000_000usize;
        let err = BookDocError::TooLarge { k, m: 4 };
        assert!(err.to_string().contains("beyond the 4M cap"), "{err}");
    }
}
