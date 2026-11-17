//! RTF (Rich Text Format) — the format every word processor since Word 2.0
//! can still write, and the one behind most "a file I can't open" support
//! requests.
//!
//! Scope: the text-bearing parts of an RTF document, which is all a reader
//! needs. Handled:
//!
//! - Group nesting (`{`/`}`) with an explicit stack, so a `\par` inside a
//!   nested group cannot leak into the paragraph buffer that opened it —
//!   the classic bug that turns a whole document into one enormous line.
//! - `\uN?` Unicode escapes (with the ANSI fallback the control's optional
//!   replacement character carries), `\'hh` byte escapes, and `\ansicpg`
//!   codepages (the shared codepage tables plus 65001 for UTF-8).
//! - Destination groups that carry content: `\line`, `\par`, `\pard`,
//!   `\sect`, `\page`, `\tab`, `\cell`, `\row`.
//! - Destinations that must be SKIPPED: the well-known metadata groups
//!   (`\fonttbl`, `\colortbl`, `\stylesheet`, `\info`, `\listtable`, …)
//!   and every `\*\…` destination the reader does not understand, whose
//!   bodies would otherwise be read aloud as gibberish.
//! - Emphasis (`\b`, `\i`, `\ul`) so a paragraph that is entirely
//!   emphasised, short and unpunctuated can open a chapter — the RTF
//!   convention for a title standing alone on its line.
//! - `\info`'s `\title`, `\author` and `\subject` for the shelf card.
//!
//! Not handled, and said so rather than half-done: `\pict` image payloads
//! (hex blobs this reader skips rather than mis-speaks) and table borders
//! beyond splitting rows at `\cell`/`\row`.
//!
//! Ported in part from the sibling speechnotes-ios `RtfParser.swift`, the
//! user's own reference implementation — minus its EPUB-conversion step:
//! the parse ends at the [`BookDoc`] and the file stays on disk verbatim.

use super::bookdoc::{xml_escape, BookChapter, BookDoc, BookDocError, BookToc};

/// Bound on one chapter's paragraphs — a document with no headings at all
/// still chunks into units the reader can open one at a time.
const CHAPTER_PARAGRAPH_LIMIT: usize = 150;

/// Parse a `.rtf` file.
pub fn parse(data: &[u8]) -> Result<BookDoc, BookDocError> {
    let reader = Reader::run(data)?;
    if reader.items.is_empty() {
        return Err(BookDocError::Malformed(
            "no readable text in the RTF document".to_string(),
        ));
    }
    let (chapters, toc) = chapterize(reader.items);
    Ok(BookDoc {
        title: reader.info_title,
        author: reader.info_author,
        chapters,
        toc,
        ..BookDoc::default()
    })
}

/// One output paragraph in progress.
#[derive(Clone, Default)]
struct Builder {
    text: String,
    /// Any emphasis formatting was ever on in this paragraph.
    emphasized: bool,
    is_bold: bool,
    is_italic: bool,
    is_underlined: bool,
    is_marked: bool,
}

impl Builder {
    fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }
}

/// A group on the stack: the state to restore on `}`.
#[derive(Clone)]
struct GroupState {
    builder: Builder,
    skip_depth: usize,
    info_key: Option<String>,
}

/// One paragraph or heading the reader emitted.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Item {
    Heading(String),
    Paragraph(String),
}

/// The streaming reader. RTF is a 7-bit syntax with `\'hh` escapes, so it
/// reads BYTES, not characters — decoding first would have already turned
/// `\'e9` into a replacement character before the codepage was known.
struct Reader {
    items: Vec<Item>,

    builder: Builder,
    stack: Vec<GroupState>,
    /// Nesting depth of a destination being skipped. > 0 means "read
    /// nothing".
    skip_depth: usize,
    /// Which `\info` field is open, if any.
    info_key: Option<String>,
    /// Raw bytes seen inside an `\info` field (the builder is bypassed
    /// while skip_depth > 0, so metadata needs its own accumulation).
    info_bytes: Vec<u8>,
    info_title: Option<String>,
    info_author: Option<String>,

    bytes: Vec<u8>,
    index: usize,
    /// Codepage from `\ansicpg`. CP1252 is the spec's floor for an RTF
    /// that does not declare one.
    codepage: u16,
    /// Single-byte text accumulated in the current group, decoded at
    /// flush.
    byte_buffer: Vec<u8>,
    /// A `\*` was just read: the next control word decides whether this
    /// group is a destination the reader skips.
    star_pending: bool,
    /// The `\ucN` state: how many fallback bytes each `\uN` is followed
    /// by. The spec's default is 1 — the `?` every writer puts after the
    /// escape.
    unicode_fallback: usize,
}

impl Reader {
    fn run(data: &[u8]) -> Result<Self, BookDocError> {
        // The signature is `{\rtfN` — the `{` plus a backslash plus `rtf`.
        // Checking the two bytes `{r` (which a naive reader does) rejects
        // every real RTF file.
        if data.len() <= 6
            || data[0] != b'{'
            || data[1] != b'\\'
            || data[2] != b'r'
            || data[3] != b't'
            || data[4] != b'f'
        {
            return Err(BookDocError::Malformed("not an RTF document".to_string()));
        }
        let mut reader = Self {
            items: Vec::new(),
            builder: Builder::default(),
            stack: Vec::new(),
            skip_depth: 0,
            info_key: None,
            info_bytes: Vec::new(),
            info_title: None,
            info_author: None,
            bytes: data.to_vec(),
            index: 0,
            codepage: 1252,
            byte_buffer: Vec::new(),
            star_pending: false,
            unicode_fallback: 1,
        };
        reader.scan();
        reader.flush_paragraph();
        Ok(reader)
    }

    fn scan(&mut self) {
        while self.index < self.bytes.len() {
            let byte = self.bytes[self.index];
            match byte {
                b'{' => {
                    // Text already read belongs to the paragraph that was
                    // open — flush it into the builder before the state
                    // snapshot, or it would glue onto the nested group's
                    // first bytes ("Outer one.nested") instead of staying
                    // where the writer put it.
                    if !self.byte_buffer.is_empty() {
                        self.decode_byte_buffer_into_builder();
                    }
                    self.stack.push(GroupState {
                        builder: self.builder.clone(),
                        skip_depth: self.skip_depth,
                        info_key: self.info_key.clone(),
                    });
                    self.star_pending = false;
                    self.index += 1;
                }
                b'}' => self.close_group(),
                b'\\' => {
                    self.index += 1;
                    // `\'hh` is a byte escape and `\*` a destination mark,
                    // not control words: handle them here so the `'` and
                    // `*` never reach `control_word` (which would read
                    // them as an empty word and lose the token).
                    match self.bytes.get(self.index) {
                        Some(b'\'') => {
                            self.index += 1;
                            self.hex_escape();
                        }
                        Some(b'*') => {
                            self.index += 1;
                            self.star_pending = true;
                        }
                        _ => self.control_word(),
                    }
                }
                b'\r' | b'\n' => {
                    self.index += 1;
                }
                _ => {
                    // Literal text, but only when not inside a destination.
                    if self.skip_depth > 0 {
                        if self.info_key.is_some() {
                            self.info_bytes.push(byte);
                        }
                        self.index += 1;
                        continue;
                    }
                    self.byte_buffer.push(byte);
                    self.index += 1;
                }
            }
        }
    }

    /// Flushes the group the `}` closes: its paragraph text becomes
    /// content and the enclosing group's state comes back.
    ///
    /// RTF has two paragraph conventions and this handles both: writers
    /// that wrap each paragraph in its own group (`{\pard …}`) and writers
    /// that rely on `\par` inside one long group. Detecting "the parent
    /// already has text" is what tells them apart — without it, the second
    /// style produces one chapter containing the whole document.
    fn close_group(&mut self) {
        if !self.byte_buffer.is_empty() {
            self.decode_byte_buffer_into_builder();
        }
        let parent_has_text = self.stack.last().map(|s| !s.builder.is_empty()).unwrap_or(false);
        if !self.builder.is_empty() {
            let finished = std::mem::take(&mut self.builder);
            if parent_has_text {
                self.emit(finished);
            } else if let Some(parent) = self.stack.last_mut() {
                // Keep accumulating into the parent: this group was a
                // formatting scope, not a paragraph. The emphasis flags
                // travel too — a bold word alone must still be able to
                // make its paragraph a title.
                parent.builder.text.push_str(&finished.text);
                parent.builder.emphasized = parent.builder.emphasized || finished.emphasized;
                parent.builder.is_marked = parent.builder.is_marked || finished.is_marked;
            }
        }
        // An `\info` field's text arrives as raw bytes inside the group
        // that held it, never through the builder (the builder is bypassed
        // while skip_depth > 0) — so it is captured at the group's close.
        if let Some(key) = self.info_key.clone() {
            if !self.info_bytes.is_empty() {
                let raw = std::mem::take(&mut self.info_bytes);
                self.capture_info(&key, &raw);
            }
        }
        match self.stack.pop() {
            Some(state) => {
                self.builder = state.builder;
                self.skip_depth = state.skip_depth;
                self.info_key = state.info_key;
            }
            None => {
                self.builder = Builder::default();
            }
        }
        self.index += 1;
    }

    // -- control words -----------------------------------------------------

    fn control_word(&mut self) {
        let mut word = String::new();
        while self.index < self.bytes.len() {
            let byte = self.bytes[self.index];
            let is_letter = byte.is_ascii_alphabetic();
            if !is_letter {
                break;
            }
            word.push(byte as char);
            self.index += 1;
        }
        // Optional signed numeric parameter.
        let mut parameter: i32 = 0;
        let mut has_parameter = false;
        if self.index < self.bytes.len() && self.bytes[self.index] == b'-' {
            has_parameter = true;
            self.index += 1;
            while self.index < self.bytes.len() && self.bytes[self.index].is_ascii_digit() {
                parameter = parameter * 10 + (self.bytes[self.index] - b'0') as i32;
                self.index += 1;
            }
            parameter = -parameter;
        } else if self.index < self.bytes.len() && self.bytes[self.index].is_ascii_digit() {
            has_parameter = true;
            while self.index < self.bytes.len() && self.bytes[self.index].is_ascii_digit() {
                parameter = parameter * 10 + (self.bytes[self.index] - b'0') as i32;
                self.index += 1;
            }
        }
        // A single space after a control word is a delimiter, not content.
        if self.index < self.bytes.len() && self.bytes[self.index] == b' ' {
            self.index += 1;
        }
        let known = self.apply(&word, parameter, has_parameter);
        if self.star_pending {
            // `\*` + unknown word: the group is a destination this reader
            // does not understand — its whole body is skipped.
            self.star_pending = false;
            if !known {
                self.skip_depth += 1;
            }
        }
    }

    /// One control word applied. Returns whether the reader knows the
    /// word — an unknown one under `\*` turns the group into a skipped
    /// destination.
    fn apply(&mut self, word: &str, parameter: i32, has_parameter: bool) -> bool {
        match word {
            "ansicpg" if has_parameter => {
                self.codepage = parameter.clamp(0, u16::MAX as i32) as u16;
                true
            }

            // Destinations to skip entirely.
            "fonttbl" | "colortbl" | "stylesheet" | "listtable" | "listoverridetable"
            | "generator" | "pict" | "object" | "themedata" | "colorschememapping"
            | "datastore" | "xmlnstbl" | "latentstyles" | "rsidtbl" | "mmathPr" | "upr"
            | "header" | "footer" | "headerl" | "headerr" | "footerl" | "footerr"
            | "footnote" | "ftnsep" | "ftnsepc" | "ftncn" | "aftnsep" | "aftnsepc"
            | "aftncn" => {
                self.skip_depth += 1;
                true
            }
            "info" => {
                self.skip_depth += 1;
                self.info_key = None;
                true
            }
            "title" | "author" | "subject" | "keywords" | "operator" | "company"
            | "category" | "doccomm" => {
                // An `\info` child: capture its text, but never read it
                // aloud.
                if self.skip_depth > 0 {
                    self.info_key = Some(word.to_string());
                    self.info_bytes.clear();
                }
                true
            }

            // A bare `\b`/`\i`/`\ul` means the property is ON — the
            // absence of a parameter is not a parameter of zero. (The
            // reference read `parameter != 0` with the default 0, so its
            // own heading heuristic never fired on standard RTF; the
            // switch-off forms are the explicit `\b0`.)
            "b" => {
                let on = !has_parameter || parameter != 0;
                self.builder.is_bold = on;
                if on {
                    self.builder.is_marked = true;
                }
                true
            }
            "i" => {
                let on = !has_parameter || parameter != 0;
                self.builder.is_italic = on;
                if on {
                    self.builder.is_marked = true;
                }
                true
            }
            "ul" | "ulw" | "uld" | "uldb" => {
                let on = !has_parameter || parameter != 0;
                self.builder.is_underlined = on;
                if on {
                    self.builder.is_marked = true;
                }
                true
            }
            "ulnone" => {
                self.builder.is_underlined = false;
                true
            }
            "plain" => {
                self.builder.is_bold = false;
                self.builder.is_italic = false;
                self.builder.is_underlined = false;
                self.builder.is_marked = false;
                true
            }

            "par" | "sect" | "page" | "line" => {
                self.flush_paragraph();
                true
            }
            "pard" => {
                // A destination change that starts a new paragraph for
                // many writers; flushing is what makes RTF's "paragraph =
                // group" and "paragraph = \par" styles both work.
                self.flush_paragraph();
                true
            }
            "tab" => {
                self.append_text(" ");
                true
            }
            "cell" => {
                self.append_text(", ");
                true
            }
            "row" => {
                self.flush_paragraph();
                true
            }

            "u" if has_parameter => {
                // RTF writes a 16-bit value; values over 32767 arrive as
                // negative numbers, which is the same bits re-read as
                // i16. A surrogate half has no character of its own and
                // is dropped.
                let scalar = u32::from((parameter as i16) as u16);
                if let Some(c) = char::from_u32(scalar) {
                    self.append_text(&c.to_string());
                }
                // The escape is followed by up to `\ucN` bytes of the
                // ANSI fallback (usually the `?` a writer emits) — those
                // bytes are the fallback, not text, and are skipped. The
                // skip stops at syntax: a fallback that never ends must
                // not eat the rest of the document.
                let mut skipped = 0;
                while skipped < self.unicode_fallback && self.index < self.bytes.len() {
                    let byte = self.bytes[self.index];
                    if matches!(byte, b'\\' | b'{' | b'}' | b'\r' | b'\n') {
                        break;
                    }
                    self.index += 1;
                    skipped += 1;
                }
                true
            }
            "uc" if has_parameter => {
                // `\ucN` SETS the fallback count the `\uN` escapes use.
                // (The reference consumed the bytes immediately here,
                // which ate the text after a bare `\uc1 `.)
                self.unicode_fallback = parameter.max(0) as usize;
                true
            }

            "emdash" => {
                self.append_text("\u{2014}");
                true
            }
            "endash" => {
                self.append_text("\u{2013}");
                true
            }
            "lquote" | "rquote" => {
                self.append_text("\u{2018}");
                true
            }
            "ldblquote" | "rdblquote" => {
                self.append_text("\u{201D}");
                true
            }
            "bullet" => {
                self.append_text("\u{2022}");
                true
            }
            "nbsp" => {
                self.append_text("\u{00A0}");
                true
            }
            _ => false,
        }
    }

    /// `\'hh` — one byte in the document's codepage.
    fn hex_escape(&mut self) {
        if self.index + 1 >= self.bytes.len() {
            self.index = self.bytes.len();
            return;
        }
        let high = hex_value(self.bytes[self.index]);
        let low = hex_value(self.bytes[self.index + 1]);
        self.index += 2;
        let (Some(high), Some(low)) = (high, low) else {
            return;
        };
        let byte = (high * 16 + low) as u8;
        if self.skip_depth > 0 {
            // Inside `\info`, byte escapes are metadata text too.
            if self.info_key.is_some() {
                self.info_bytes.push(byte);
            }
            return;
        }
        self.byte_buffer.push(byte);
    }

    // -- text accumulation ---------------------------------------------------

    fn append_text(&mut self, value: &str) {
        if self.skip_depth > 0 {
            return;
        }
        if !self.byte_buffer.is_empty() {
            self.decode_byte_buffer_into_builder();
        }
        self.builder.text.push_str(value);
        if self.builder.is_bold || self.builder.is_italic || self.builder.is_underlined {
            self.builder.emphasized = true;
        }
    }

    /// The single-byte buffer accumulates `\'hh` and other raw 8-bit
    /// bytes; it is decoded once, in the document's codepage, when text is
    /// needed.
    fn decode_byte_buffer_into_builder(&mut self) {
        if self.byte_buffer.is_empty() {
            return;
        }
        let buffer = std::mem::take(&mut self.byte_buffer);
        let decoded = match self.codepage {
            65001 => String::from_utf8_lossy(&buffer).into_owned(),
            page => super::codepages::decode_named(&page.to_string(), &buffer)
                .unwrap_or_else(|| String::from_utf8_lossy(&buffer).into_owned()),
        };
        self.builder.text.push_str(&decoded);
        if self.builder.is_bold || self.builder.is_italic || self.builder.is_underlined {
            self.builder.emphasized = true;
        }
    }

    // -- paragraphs ----------------------------------------------------------

    fn flush_paragraph(&mut self) {
        if !self.byte_buffer.is_empty() {
            self.decode_byte_buffer_into_builder();
        }
        let finished = std::mem::take(&mut self.builder);
        if finished.is_empty() {
            return;
        }
        self.emit(finished);
    }

    fn emit(&mut self, value: Builder) {
        // While a destination is being skipped there is nothing to emit:
        // the builder is bypassed entirely, so this only fires for a group
        // that closed while still marked as skipped.
        if self.skip_depth > 0 {
            return;
        }
        let text = tidy(&value.text);
        if text.is_empty() {
            return;
        }
        // An emphasised, short, unpunctuated line standing on its own is a
        // heading — the same convention the FB2 and HTML readers use.
        let looks_like_heading = value.is_marked
            && value.emphasized
            && text.chars().count() <= 120
            && !text.ends_with(['.', ',', ';', '!', '?']);
        if looks_like_heading {
            self.items.push(Item::Heading(text));
        } else {
            self.items.push(Item::Paragraph(text));
        }
    }

    fn capture_info(&mut self, key: &str, raw: &[u8]) {
        let decoded = match self.codepage {
            65001 => String::from_utf8_lossy(raw).into_owned(),
            page => super::codepages::decode_named(&page.to_string(), raw)
                .unwrap_or_else(|| String::from_utf8_lossy(raw).into_owned()),
        };
        let value = tidy(&decoded);
        if value.is_empty() {
            return;
        }
        match key {
            "title" if self.info_title.is_none() => self.info_title = Some(value),
            "author" if self.info_author.is_none() => self.info_author = Some(value),
            _ => {}
        }
    }
}

fn hex_value(byte: u8) -> Option<i32> {
    match byte {
        b'0'..=b'9' => Some((byte - b'0') as i32),
        b'A'..=b'F' => Some((byte - b'A') as i32 + 10),
        b'a'..=b'f' => Some((byte - b'a') as i32 + 10),
        _ => None,
    }
}

/// Whitespace and NBSP tidy — collapse the indentation and line wraps an
/// RTF writer scatters through a paragraph. An RTF writer's empty
/// paragraph is often a couple of stray spaces plus a tab; the tidy
/// handles that too.
fn tidy(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_space = false;
    for c in text.chars() {
        if c.is_whitespace() || c == '\u{00A0}' {
            if !in_space {
                out.push(' ');
                in_space = true;
            }
        } else {
            out.push(c);
            in_space = false;
        }
    }
    out.trim().to_string()
}

// -- chapterization ----------------------------------------------------------

/// RTF carries no outline structure, so chapters are the emphasised
/// standalone lines; everything between them is one chapter. A document
/// with none chunks into ~150-paragraph chapters.
fn chapterize(items: Vec<Item>) -> (Vec<BookChapter>, Vec<BookToc>) {
    let mut chapters: Vec<BookChapter> = Vec::new();
    let mut toc: Vec<BookToc> = Vec::new();
    let mut title: Option<String> = None;
    let mut html = String::new();
    let mut paragraph_count = 0usize;

    fn flush(
        chapters: &mut Vec<BookChapter>,
        toc: &mut Vec<BookToc>,
        title: &mut Option<String>,
        html: &mut String,
        paragraph_count: &mut usize,
    ) {
        if !html.is_empty() || title.is_some() {
            let label = title
                .clone()
                .unwrap_or_else(|| format!("Chapter {}", chapters.len() + 1));
            toc.push(BookToc {
                label,
                chapter_index: chapters.len(),
                depth: 0,
            });
            chapters.push(BookChapter {
                title: title.clone(),
                html: std::mem::take(html),
            });
        }
        *paragraph_count = 0;
    }

    for item in items {
        match item {
            Item::Heading(text) => {
                flush(
                    &mut chapters,
                    &mut toc,
                    &mut title,
                    &mut html,
                    &mut paragraph_count,
                );
                title = Some(text);
            }
            Item::Paragraph(text) => {
                html.push_str("<p>");
                html.push_str(&xml_escape(&text));
                html.push_str("</p>");
                paragraph_count += 1;
            }
        }
        if paragraph_count >= CHAPTER_PARAGRAPH_LIMIT {
            // The bound: the title carries onto the continuation chapter
            // so the contents list never shows "Chapter N" mid-section.
            let carried = title.take();
            flush(
                &mut chapters,
                &mut toc,
                &mut title,
                &mut html,
                &mut paragraph_count,
            );
            title = carried;
        }
    }
    flush(
        &mut chapters,
        &mut toc,
        &mut title,
        &mut html,
        &mut paragraph_count,
    );

    (chapters, toc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paragraphs_and_heading_groups_come_out() {
        let rtf = b"{\\rtf1\\ansi{\\b Title Line}\\par First paragraph.\\par Second.\\par}";
        let doc = parse(rtf).unwrap();
        // One heading opens one chapter; everything until the next one
        // belongs to it.
        assert_eq!(doc.chapters.len(), 1);
        assert_eq!(doc.chapters[0].title.as_deref(), Some("Title Line"));
        assert!(doc.chapters[0].html.contains("First paragraph."));
        assert!(doc.chapters[0].html.contains("Second."));
        assert_eq!(doc.toc[0].label, "Title Line");
    }

    #[test]
    fn hex_escapes_and_the_default_codepage_decode() {
        // \'e9 is é in cp1252 — the floor an RTF without \ansicpg uses.
        let rtf = b"{\\rtf1 Caf\\'e9.}";
        let doc = parse(rtf).unwrap();
        assert!(doc.chapters[0].html.contains("Café."), "{}", doc.chapters[0].html);
    }

    #[test]
    fn an_ansicpg_names_its_codepage() {
        // "Привет" in windows-1251.
        let mut rtf = b"{\\rtf1\\ansi\\ansicpg1251 ".to_vec();
        rtf.extend_from_slice(&[0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2]);
        rtf.extend_from_slice(b".}");
        let doc = parse(&rtf).unwrap();
        assert!(
            doc.chapters[0].html.contains("Привет"),
            "{}",
            doc.chapters[0].html
        );
    }

    #[test]
    fn unicode_escapes_decode_and_swallow_their_fallback() {
        // \u8212? — an em dash with the `?` fallback consumed; \uc1 tells
        // the reader to skip the one ANSI byte after each escape.
        let rtf = b"{\\rtf1\\uc1 A\\u8212?B}";
        let doc = parse(rtf).unwrap();
        let text = &doc.chapters[0].html;
        assert!(text.contains("A\u{2014}B"), "{text}");
        assert!(!text.contains('?'), "{text}");
    }

    #[test]
    fn metadata_destinations_are_skipped_and_info_is_captured() {
        let rtf = b"{\\rtf1{\\fonttbl{\\f0 Times;}}{\\colortbl;\\red0\\green0\\blue0;}\
{\\info{\\title Info Title}{\\author Info Author}}\\par Body text.}";
        let doc = parse(rtf).unwrap();
        assert_eq!(doc.title.as_deref(), Some("Info Title"));
        assert_eq!(doc.author.as_deref(), Some("Info Author"));
        let text = doc.chapters.iter().map(|c| c.html.as_str()).collect::<String>();
        assert!(text.contains("Body text."), "{text}");
        assert!(!text.contains("Times"), "{text}");
        assert!(!text.contains("red0"), "{text}");
        assert!(!text.contains("Info Title"), "{text}");
    }

    #[test]
    fn an_unknown_star_destination_is_skipped() {
        // \*\shpinst is a drawing destination no reader needs; its body
        // must not be read aloud.
        let rtf = b"{\\rtf1\\par Keep me.\\par{\\*\\shpinst secret drawing words}\\par After.}";
        let doc = parse(rtf).unwrap();
        let text = doc.chapters.iter().map(|c| c.html.as_str()).collect::<String>();
        assert!(text.contains("Keep me."), "{text}");
        assert!(text.contains("After."), "{text}");
        assert!(!text.contains("secret"), "{text}");
    }

    #[test]
    fn a_nested_group_cannot_leak_its_paragraph() {
        // The classic bug: a \par inside a nested group starting a new
        // paragraph in the OUTER buffer, welding the document into one
        // line.
        let rtf = b"{\\rtf1\\par Outer one.{\\b nested\\par Inner}\\par Outer two.}";
        let doc = parse(rtf).unwrap();
        let text = doc.chapters.iter().map(|c| c.html.as_str()).collect::<String>();
        assert!(text.contains("Outer one."), "{text}");
        assert!(text.contains("Inner"), "{text}");
        assert!(text.contains("Outer two."), "{text}");
    }

    #[test]
    fn cells_join_and_rows_break() {
        let rtf = b"{\\rtf1\\trowd Apples\\cell 12\\cell\\row Pears\\cell 4\\cell\\row}";
        let doc = parse(rtf).unwrap();
        let text = &doc.chapters[0].html;
        assert!(text.contains("Apples, 12"), "{text}");
        assert!(text.contains("Pears, 4"), "{text}");
    }

    #[test]
    fn emphasis_and_entity_words_decode() {
        let rtf = b"{\\rtf1 an em\\emdash dash and a\\bullet dot.}";
        let doc = parse(rtf).unwrap();
        let text = &doc.chapters[0].html;
        assert!(text.contains("em\u{2014}dash"), "{text}");
        assert!(text.contains("\u{2022}dot"), "{text}");
    }

    #[test]
    fn not_an_rtf_is_refused_and_empty_is_refused() {
        let err = parse(b"{\\rtf ").unwrap_err(); // too short to be one
        assert!(err.to_string().contains("not an RTF document"), "{err}");
        let err = parse(b"hello world").unwrap_err();
        assert!(err.to_string().contains("not an RTF document"), "{err}");
        // A valid RTF with no text at all.
        let err = parse(b"{\\rtf1\\ansi{\\fonttbl{\\f0 Times;}}}").unwrap_err();
        assert!(err.to_string().contains("no readable text"), "{err}");
    }

    #[test]
    fn a_headingless_document_chunks_by_paragraph_count() {
        let paragraph = "Some prose sentence.";
        let mut rtf = b"{\\rtf1 ".to_vec();
        for _ in 0..400 {
            rtf.extend_from_slice(paragraph.as_bytes());
            rtf.extend_from_slice(b"\\par ");
        }
        rtf.extend_from_slice(b"}");
        let doc = parse(&rtf).unwrap();
        assert!(doc.chapters.len() >= 3, "{} chapters", doc.chapters.len());
        assert!(doc.chapters.iter().all(|c| !c.html.is_empty()));
    }
}
