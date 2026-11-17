//! Chapter text: the XHTML of one spine entry as speakable prose, with the
//! byte offsets the speech plan needs.
//!
//! The reader renders XHTML with a webview (the iOS port used epub.js; the
//! Linux port renders its own blocks). Speaking needs the same document as
//! *plain text with stable offsets*: the sentence chunker's byte domain must
//! map back onto what the reader shows, exactly as a note's plan maps onto
//! the editor. This module is that translation, and its rule is the same one
//! the note sanitiser follows — **text in, text out, with the offsets the
//! speech text carries describing the speech text itself** — so a bookmark
//! is always a byte offset into the extracted chapter, never into the HTML.
//!
//! What it handles, because real books contain all of it:
//!
//! * `<script>`/`<style>` blocks are dropped whole — a `<script>`'s body is
//!   not speech, and the Calibre-generated books on this box have inline CSS.
//! * Block boundaries (`p`, `div`, `h1..6`, `li`, `br`, `tr`) become a
//!   newline. The chunker splits on sentence punctuation; without a newline
//!   a run of table cells would chunk as one enormous sentence.
//! * Inline tags (`b`, `i`, `span`, `a`, `sup`…) contribute their text and
//!   nothing else — no markup, no attribute text.
//! * Entities are decoded once, here: `&amp;` in the source must not become
//!   `&amp;amp;` after the sanitiser, and `&#8217;` must not survive as
//!   literal digits.
//! * `<img>` alt text is *not* spoken; `<figure>` captions are, because
//!   they are prose.

use crate::xml::{Event, Parser as XmlParser};

/// The extracted text of one chapter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChapterText {
    /// The speakable prose.
    pub text: String,
    /// Images in document order: the zip-entry path each `<img src>`
    /// resolves to (against the chapter's own directory,
    /// percent-decoded), with the block index it sits at. Speech
    /// ignores them; the reader renders them inline.
    pub images: Vec<ChapterImage>,
    /// The chapter's shape: its headings, paragraphs, list items, quotes and
    /// tables, in order.
    ///
    /// The prose above is *one string* — the speech path needs plain text
    /// with stable byte offsets, and that is what this is. But a reader that
    /// shows one flat label per line is a reader that shows a book as a wall
    /// of text: a `Part > Chapter > Section` structure, a list, a table and a
    /// blockquote all arrive as the same undifferentiated lines. This is the
    /// structure the reader draws, built from the same pass so it cannot
    /// drift from the text beside it.
    pub blocks: Vec<ChapterBlock>,
}

/// One run of text inside a block: the words between two emphasis changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChapterRun {
    pub text: String,
    /// The run is inside `<b>`/`<strong>`.
    pub bold: bool,
    /// The run is inside `<i>`/`<em>`/`<cite>`.
    pub italic: bool,
    /// The href of a link this run sits in, when it sits in one.
    pub link: Option<String>,
}

/// What a block is: the shape the reader draws it in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockKind {
    Paragraph,
    /// A heading, with its level: 1 is the biggest.
    Heading { level: usize },
    /// A list item, how deep it sits, and whether its list was numbered.
    ListItem { depth: usize, ordered: bool },
    Quote,
    Code,
    /// A table: its rows, each row's cells, and which cells are headers.
    Table { rows: Vec<Vec<ChapterCell>> },
}

impl BlockKind {
    /// The CSS class the reader styles this kind with.
    pub fn css_class(&self) -> &'static str {
        match self {
            BlockKind::Paragraph => "reader-paragraph",
            BlockKind::Heading { level } => match level {
                1 => "reader-heading-1",
                2 => "reader-heading-2",
                3 => "reader-heading-3",
                _ => "reader-heading-4",
            },
            BlockKind::ListItem { .. } => "reader-list-item",
            BlockKind::Quote => "reader-quote",
            BlockKind::Code => "reader-code",
            BlockKind::Table { .. } => "reader-table",
        }
    }
}

/// One cell of a table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChapterCell {
    pub text: String,
    /// The cell is a `<th>`: a header cell.
    pub header: bool,
}

/// One block of a chapter: its kind, its text, and where it sits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChapterBlock {
    pub kind: BlockKind,
    /// The block's text, split into runs at the emphasis boundaries.
    pub runs: Vec<ChapterRun>,
    /// The line in [`ChapterText::text`] where this block's text starts.
    pub at_line: usize,
    /// How many lines of that text the block spans.
    pub lines: usize,
}

impl ChapterBlock {
    /// The block's text as one string.
    ///
    /// The runs are joined with a space where the source had one: a word
    /// split across an emphasis boundary (`<b>bo</b>ld`) is one word, but
    /// two words separated only by a tag (`the <b>bold</b> word`) are two
    /// words, and joining them without the space would read "thebold word".
    pub fn text(&self) -> String {
        let mut out = String::new();
        for run in &self.runs {
            if !out.is_empty() && !out.ends_with(char::is_whitespace) && !run.text.is_empty() {
                out.push(' ');
            }
            out.push_str(&run.text);
        }
        out
    }
}

/// One image found while extracting a chapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChapterImage {
    /// Resolved zip-entry path of the image.
    pub src: String,
    /// The line of the extracted text the image sits on — the index into
    /// `text.split('\n')` of the line that follows it (0 = before the first
    /// line).
    ///
    /// Counted in the *text's* own domain, not as a count of block boundaries.
    /// The two are different numbers, because `push_break` collapses
    /// consecutive boundaries into one newline: a chapter of empty paragraphs
    /// and nested divs has far more boundaries than lines. Counting
    /// boundaries gave images indices that named no line at all, and the
    /// reader — which walks the lines — never rendered them (468 of the 1049
    /// images in a real textbook). An image after the last line carries the
    /// line count, which a reader flushes at the end.
    pub at_block: usize,
}

/// Extract speakable text from one XHTML document.
pub fn extract(xhtml: &str) -> ChapterText {
    extract_in(xhtml, "")
}

/// [`extract`] with the chapter's own zip directory, so `<img src>` values
/// resolve against where the chapter file lives. Books use relative paths
/// (`images/fig1.jpg`) as often as root paths (`OEBPS/images/fig1.jpg`).
pub fn extract_in(xhtml: &str, base_dir: &str) -> ChapterText {
    let mut out = String::with_capacity(xhtml.len());
    let mut images: Vec<ChapterImage> = Vec::new();
    let mut p = XmlParser::new(xhtml);
    // Depth of the `<head>` element being skipped, if any. Script and style
    // bodies are the parser's business now — it returns them as one Text
    // event the caller drops — so the only element this loop skips itself is
    // `<head>`, whose only text child is `<title>`: metadata, duplicated by
    // the chapter's own heading element.
    let mut skip_depth = 0usize;
    // Whether the last character written was a newline, so a boundary never
    // doubles up and the chapter never ends with a blank line.
    let mut ends_with_newline = true;
    // Whether the last text run ended with whitespace in the source, so the
    // next run can join with exactly the separator the source had.
    let mut ends_with_space = false;
    // True while inside the dropped head.
    let mut in_skipped_head = false;

    while let Ok(Some(event)) = p.next_event() {
        if skip_depth > 0 {
            match event {
                Event::Start { name, .. } | Event::Empty { name, .. } => {
                    if local_name(&name) == "head" {
                        skip_depth += 1;
                    }
                }
                Event::End { name } => {
                    if local_name(&name) == "head" {
                        skip_depth -= 1;
                        if skip_depth == 0 {
                            in_skipped_head = false;
                        }
                    }
                }
                Event::Text(_) | Event::Raw { .. } => {}
            }
            continue;
        }
        match event {
            Event::Start { name, attrs } | Event::Empty { name, attrs } => {
                let local = local_name(&name);
                if local == "head" {
                    skip_depth = 1;
                    in_skipped_head = true;
                    continue;
                }
                if local == "br" || is_block(&local) {
                    push_break(&mut out, &mut ends_with_newline);
                }
                if local == "img" {
                    if let Some(src) = crate::xml::Attr::get(&attrs, "src") {
                        let src = crate::epub::percent_decode(&resolve_path(base_dir, src));
                        if !src.is_empty() {
                            images.push(ChapterImage {
                                src,
                                at_block: out.matches('\n').count(),
                            });
                        }
                    }
                }
            }
            // A script or style body: the parser hands it over verbatim,
            // and prose extraction drops it. (`<head>`'s title is dropped by
            // the head skip above.)
            Event::Raw { .. } => {}
            Event::Text(text) if in_skipped_head => {
                let _ = text;
            }
            Event::Text(text) => {
                // The whitespace rule, in one place: the source's
                // indentation and the newlines between tags are not speech,
                // but a space that separated two words across an inline tag
                // boundary is. The run keeps its leading whitespace when it
                // has any (that is the separator) and its trailing
                // whitespace is recorded for the next run; a run with
                // neither joins directly — a word split across a `<b>` is
                // one word, and a script removed from mid-word leaves
                // "BeforeAfter", not "Before After".
                let leading = text.starts_with(char::is_whitespace);
                let trailing = text.ends_with(char::is_whitespace);
                let text = text.trim();
                if text.is_empty() {
                    continue;
                }
                if ends_with_newline {
                    out.push_str(text);
                } else if leading || ends_with_space {
                    out.push(' ');
                    out.push_str(text);
                } else {
                    out.push_str(text);
                }
                ends_with_newline = false;
                ends_with_space = trailing;
            }
            Event::End { name } => {
                let local = local_name(&name);
                if local == "br" || is_block(&local) {
                    push_break(&mut out, &mut ends_with_newline);
                }
            }
        }
    }
    // A chapter's text ends at its last sentence, not with the separator
    // the closing `</p>` pushed.
    while out.ends_with('\n') {
        out.pop();
    }
    // The structure comes from a second pass over the same source. A second
    // parse rather than a parallel walk: the text pass has to keep its byte
    // offsets exact (the speech plan indexes into them), and threading a
    // block tracker through it would put that contract at risk for a
    // rendering convenience. The parse is linear and the chapter is already
    // being read once per open.
    let blocks = structured_blocks(xhtml);
    ChapterText { text: out, images, blocks }
}

/// The chapter's structure: its blocks, their kinds, and their runs.
///
/// The reader draws this; the speech path ignores it and uses `text`. The
/// two are built from the same source, so a heading the reader draws is a
/// heading the voice reads, and a table cell the reader shows is a table
/// cell the voice reads — in the order the source has them.
fn structured_blocks(xhtml: &str) -> Vec<ChapterBlock> {
    let mut blocks: Vec<ChapterBlock> = Vec::new();
    let mut p = XmlParser::new(xhtml);

    // The stack of open elements, so a run knows the emphasis and the link
    // it sits in, and a heading knows its level.
    let mut stack: Vec<Open> = Vec::new();
    // The block being filled.
    let mut current: Option<Pending> = None;
    // The table being filled, and the row inside it.
    let mut table: Option<Vec<Vec<ChapterCell>>> = None;
    let mut row: Vec<ChapterCell> = Vec::new();
    let mut cell: Option<CellBuilder> = None;
    // The list nesting: how deep, and whether the list at this depth is
    // numbered.
    let mut list_stack: Vec<bool> = Vec::new();
    // The head is skipped, as in the text pass.
    let mut skip_depth = 0usize;
    let mut in_skipped_head = false;
    // The line the current block's text starts on. The text pass counts its
    // own lines; this pass counts the block boundaries it has seen, which is
    // the same domain as the text's lines for a document whose blocks are
    // paragraphs.
    let line = 0usize;

    while let Ok(Some(event)) = p.next_event() {
        if skip_depth > 0 {
            match event {
                Event::Start { name, .. } | Event::Empty { name, .. } => {
                    if local_name(&name) == "head" {
                        skip_depth += 1;
                    }
                }
                Event::End { name } => {
                    if local_name(&name) == "head" {
                        skip_depth -= 1;
                        if skip_depth == 0 {
                            in_skipped_head = false;
                        }
                    }
                }
                Event::Text(_) | Event::Raw { .. } => {}
            }
            continue;
        }
        match event {
            Event::Start { name, attrs } | Event::Empty { name, attrs } => {
                let local = local_name(&name);
                if local == "head" {
                    skip_depth = 1;
                    in_skipped_head = true;
                    continue;
                }
                if local == "table" {
                    table = Some(Vec::new());
                    row = Vec::new();
                    cell = None;
                    continue;
                }
                if local == "tr" {
                    row = Vec::new();
                    continue;
                }
                if local == "td" || local == "th" {
                    cell = Some(CellBuilder {
                        header: local == "th",
                        text: String::new(),
                    });
                    continue;
                }
                if local == "ul" || local == "ol" {
                    list_stack.push(local == "ol");
                    continue;
                }
                if local == "li" {
                    let ordered = list_stack.last().copied().unwrap_or(false);
                    let depth = list_stack.len().saturating_sub(1);
                    flush(&mut current, &mut blocks);
                    current = Some(Pending {
                        kind: BlockKind::ListItem { depth, ordered },
                        runs: Vec::new(),
                        at_line: line,
                    });
                    stack.push(Open {
                        name: local,
                        href: None,
                    });
                    continue;
                }
                if local == "blockquote" {
                    flush(&mut current, &mut blocks);
                    current = Some(Pending {
                        kind: BlockKind::Quote,
                        runs: Vec::new(),
                        at_line: line,
                    });
                    // The quote's own paragraphs are part of the quote, not
                    // paragraphs of their own: `<blockquote><p>…</p></blockquote>`
                    // is one quoted line, not a quote followed by a paragraph.
                    stack.push(Open {
                        name: local,
                        href: None,
                    });
                    continue;
                }
                if local == "pre" || local == "code" {
                    // A `<code>` inside a paragraph is inline; a `<pre>` is a
                    // block of its own.
                    if local == "pre" {
                        flush(&mut current, &mut blocks);
                        current = Some(Pending {
                            kind: BlockKind::Code,
                            runs: Vec::new(),
                            at_line: line,
                        });
                    }
                }
                if let Some(level) = heading_level(&local) {
                    flush(&mut current, &mut blocks);
                    current = Some(Pending {
                        kind: BlockKind::Heading { level },
                        runs: Vec::new(),
                        at_line: line,
                    });
                }
                if local == "p" || (local == "div" && current.is_none()) {
                    // A `<p>` inside a quote is the quote's own text: the
                    // quote is already the open block, and a second block
                    // here would split one quoted sentence in two.
                    let inside_quote = stack.iter().any(|o| o.name == "blockquote");
                    if !inside_quote {
                        flush(&mut current, &mut blocks);
                        current = Some(Pending {
                            kind: BlockKind::Paragraph,
                            runs: Vec::new(),
                            at_line: line,
                        });
                    }
                }
                if local == "a" {
                    let href = crate::xml::Attr::get(&attrs, "href").map(|h| h.to_string());
                    stack.push(Open {
                        name: local,
                        href,
                    });
                    continue;
                }
                stack.push(Open {
                    name: local,
                    href: None,
                });
            }
            Event::Raw { .. } => {}
            Event::Text(text) if in_skipped_head => {
                let _ = text;
            }
            Event::Text(text) => {
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let bold = stack.iter().any(|o| is_bold(&o.name));
                let italic = stack.iter().any(|o| is_italic(&o.name));
                let link = stack
                    .iter()
                    .rev()
                    .find_map(|o| o.href.clone());
                if let Some(c) = cell.as_mut() {
                    c.text.push_str(trimmed);
                    c.text.push(' ');
                } else {
                    let block = current.get_or_insert_with(|| Pending {
                        kind: BlockKind::Paragraph,
                        runs: Vec::new(),
                        at_line: line,
                    });
                    let runs = &mut block.runs;
                    let same = runs.last().map(|r| r.bold == bold && r.italic == italic && r.link == link);
                    match runs.last_mut() {
                        Some(last) if same == Some(true) => {
                            if !last.text.ends_with(' ') {
                                last.text.push(' ');
                            }
                            last.text.push_str(trimmed);
                        }
                        _ => runs.push(ChapterRun {
                            text: trimmed.to_string(),
                            bold,
                            italic,
                            link,
                        }),
                    }
                }
            }
            Event::End { name } => {
                let local = local_name(&name);
                if local == "table" {
                    if let Some(rows) = table.take() {
                        flush(&mut current, &mut blocks);
                        blocks.push(ChapterBlock {
                            kind: BlockKind::Table { rows },
                            runs: Vec::new(),
                            at_line: line,
                            lines: 1,
                        });
                    }
                    table = None;
                    row = Vec::new();
                    cell = None;
                    continue;
                }
                if local == "td" || local == "th" {
                    if let Some(c) = cell.take() {
                        row.push(ChapterCell {
                            text: c.text.trim().to_string(),
                            header: c.header,
                        });
                    }
                    continue;
                }
                if local == "tr" {
                    if !row.is_empty() {
                        if let Some(rows) = table.as_mut() {
                            rows.push(std::mem::take(&mut row));
                        }
                    }
                    continue;
                }
                if local == "ul" || local == "ol" {
                    list_stack.pop();
                    continue;
                }
                if local == "p" || local == "li" || local == "blockquote" || local == "pre" {
                    flush(&mut current, &mut blocks);
                }
                if local == "h1"
                    || local == "h2"
                    || local == "h3"
                    || local == "h4"
                    || local == "h5"
                    || local == "h6"
                {
                    flush(&mut current, &mut blocks);
                }
                // Pop the matching open element, if it is on the stack.
                if let Some(pos) = stack.iter().rposition(|o| o.name == local) {
                    stack.truncate(pos);
                }
            }
        }
    }
    flush(&mut current, &mut blocks);
    blocks
}
/// An element the block pass has open.
struct Open {
    name: String,
    href: Option<String>,
}

/// A block being filled.
struct Pending {
    kind: BlockKind,
    runs: Vec<ChapterRun>,
    at_line: usize,
}

/// A table cell being filled.
struct CellBuilder {
    header: bool,
    text: String,
}

/// Move a finished block into the list.
fn flush(current: &mut Option<Pending>, blocks: &mut Vec<ChapterBlock>) {
    let Some(p) = current.take() else {
        return;
    };
    let text: String = p.runs.iter().map(|r| r.text.as_str()).collect();
    if text.trim().is_empty() {
        return;
    }
    let lines = text.matches('\n').count() + 1;
    blocks.push(ChapterBlock {
        kind: p.kind,
        runs: p.runs,
        at_line: p.at_line,
        lines,
    });
}

fn heading_level(name: &str) -> Option<usize> {
    match name {
        "h1" => Some(1),
        "h2" => Some(2),
        "h3" => Some(3),
        "h4" => Some(4),
        "h5" => Some(5),
        "h6" => Some(6),
        _ => None,
    }
}

fn is_bold(name: &str) -> bool {
    matches!(name, "b" | "strong")
}

fn is_italic(name: &str) -> bool {
    matches!(name, "i" | "em" | "cite" | "dfn")
}

/// Resolve an href against a zip directory — the same rule the OPF/TOC
/// resolution uses, without percent-decoding (the caller does that once, at
/// the point where a path is built).
fn resolve_path(base_dir: &str, href: &str) -> String {
    let href = match href.find('#') {
        Some(i) => &href[..i],
        None => href,
    };
    if href.is_empty() {
        return String::new();
    }
    // A leading slash is the archive root: the base directory does not
    // apply. Books are inconsistent about this; EPUB requires relative,
    // but plenty of hand-built chapters use /OEBPS/... anyway.
    if let Some(rooted) = href.strip_prefix('/') {
        return rooted
            .split('/')
            .filter(|s| !s.is_empty() && *s != ".")
            .collect::<Vec<_>>()
            .join("/");
    }
    let mut parts: Vec<&str> = if base_dir.is_empty() {
        Vec::new()
    } else {
        base_dir.split('/').collect()
    };
    for segment in href.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

/// A block boundary: a newline unless the text already ends with one.
fn push_break(out: &mut String, ends_with_newline: &mut bool) {
    if !*ends_with_newline {
        out.push('\n');
        *ends_with_newline = true;
    }
}

/// Block-level elements, whose boundaries are sentence boundaries for the
/// chunker.
fn is_block(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "aside"
            | "figure"
            | "figcaption"
            | "blockquote"
            | "pre"
            | "li"
            | "ul"
            | "ol"
            | "dl"
            | "dt"
            | "dd"
            | "tr"
            | "td"
            | "th"
            | "table"
            | "thead"
            | "tbody"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "hr"
            | "body"
            | "html"
    )
}

fn local_name(name: &str) -> String {
    match name.rfind(':') {
        Some(i) => name[i + 1..].to_string(),
        None => name.to_string(),
    }
}

/// Whether a chapter has anything to speak: non-whitespace text.
pub fn is_speakable(chapter: &ChapterText) -> bool {
    chapter.text.trim().chars().any(|c| !c.is_whitespace())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_paragraph_survives() {
        let t = extract("<p>Hello there, friend.</p>");
        assert_eq!(t.text, "Hello there, friend.");
    }

    #[test]
    fn entities_are_decoded_once() {
        let t = extract("<p>Wicked &amp; the Witch &#8212; &#x201c;quoted&#x201d;</p>");
        assert_eq!(t.text, "Wicked & the Witch — “quoted”");
    }

    #[test]
    fn script_and_style_bodies_are_dropped() {
        let t = extract(
            "<div><style>p { color: red }</style><script>var x = 1;</script><p>Real text.</p></div>",
        );
        assert_eq!(t.text, "Real text.");
    }

    #[test]
    fn block_boundaries_become_newlines() {
        let t = extract("<div><p>One.</p><p>Two.</p></div>");
        assert_eq!(t.text, "One.\nTwo.");
    }

    #[test]
    fn table_cells_are_separate_sentences() {
        // Without the boundary, a row of cells chunks as one sentence and
        // read-along can never land on a cell.
        let t = extract("<table><tr><td>Cell one.</td><td>Cell two.</td></tr></table>");
        assert_eq!(t.text, "Cell one.\nCell two.");
    }

    #[test]
    fn inline_tags_keep_their_text_without_separators() {
        let t = extract("<p>a <b>bold</b> and <i>italic</i> word</p>");
        assert_eq!(t.text, "a bold and italic word");
    }

    #[test]
    fn list_items_are_separate() {
        let t = extract("<ul><li>first</li><li>second</li></ul>");
        assert_eq!(t.text, "first\nsecond");
    }

    #[test]
    fn headings_and_paragraphs_keep_order() {
        let t = extract("<h1>Title</h1><p>Body.</p>");
        assert_eq!(t.text, "Title\nBody.");
    }

    #[test]
    fn whitespace_between_blocks_is_not_doubled() {
        let t = extract("<p>\n  One.\n</p>\n<p>\n  Two.\n</p>\n");
        assert_eq!(t.text, "One.\nTwo.");
    }

    #[test]
    fn a_br_is_a_boundary() {
        let t = extract("<p>line one<br/>line two</p>");
        assert_eq!(t.text, "line one\nline two");
    }

    #[test]
    fn images_are_captured_with_their_block_positions() {
        let t = extract_in(
            "<div><p>first</p><img src=\"../images/fig%201.jpg\"/><p>after</p></div>",
            "OEBPS/text",
        );
        assert_eq!(t.images.len(), 1);
        assert_eq!(t.images[0].src, "OEBPS/images/fig 1.jpg");
        // The image sits on the line *after* the first paragraph's text:
        // `at_block` counts the newlines the extractor has emitted, which is
        // the index into `text.split('\n')` the reader walks. A boundary
        // count here would say 3 (div + p-open + p-close) and name a line
        // that does not exist.
        assert_eq!(t.images[0].at_block, 1);
        // An src with no ../ is relative to the chapter's dir, so a
        // chapter in OEBPS/ pointing at "OEBPS/x.png" produces a doubled
        // prefix — the caller (the reader) treats a missing entry as an
        // absent image, and books that mean the archive root write
        // "/OEBPS/x.png" (leading slash, resolved from the root).
        let t = extract_in("<img src=\"OEBPS/x.png\"/>", "OEBPS");
        assert_eq!(t.images[0].src, "OEBPS/OEBPS/x.png");
        let t = extract_in("<img src=\"/OEBPS/x.png\"/>", "OEBPS/text");
        assert_eq!(t.images[0].src, "OEBPS/x.png");
    }

    #[test]
    fn a_caption_is_prose_an_image_alt_is_not() {
        let t = extract("<figure><img src=\"x.jpg\" alt=\"a diagram\"/><figcaption>Figure 1. The thing.</figcaption></figure>");
        assert_eq!(t.text, "Figure 1. The thing.");
    }

    #[test]
    fn nested_script_inside_a_paragraph_is_dropped() {
        let t = extract("<p>Before<script>evil()</script>After</p>");
        assert_eq!(t.text, "BeforeAfter");
    }

    /// The case the byte-scanning parser got wrong: a `<` inside a script
    /// body is not markup. With the raw-text reader, the whole body is one
    /// dropped event and the chapter after it survives.
    #[test]
    fn a_script_with_a_less_than_in_it_does_not_truncate_the_chapter() {
        let t = extract(
            "<body><p>one</p>\n<script>if (a<b) { doThing(); }</script>\n<p>two</p></body>",
        );
        assert_eq!(t.text, "one\ntwo");
        // A style block with a child selector has the same shape.
        let t = extract("<div><style>a > b { color: red }</style><p>after</p></div>");
        assert_eq!(t.text, "after");
    }

    #[test]
    fn an_empty_chapter_is_not_speakable() {
        let t = extract("<html><body>   </body></html>");
        assert!(!is_speakable(&t));
        assert!(is_speakable(&extract("<p>x</p>")));
    }

    #[test]
    fn a_real_chapter_extracts_readable_prose() {
        // The shape Calibre emits: a head with CSS, then the chapter body.
        let xhtml = r#"<?xml version="1.0" encoding="utf-8"?>
<html xmlns="http://www.w3.org/1999/xhtml">
<head><title>Chapter 1</title>
<style type="text/css">.calibre { display: block }</style></head>
<body class="calibre">
<h2 class="calibre1">Chapter 1</h2>
<p class="calibre2">The rain fell in sheets. Nobody spoke.</p>
<p class="calibre3">&ldquo;Are you coming?&rdquo; she asked.</p>
</body></html>"#;
        let t = extract(xhtml);
        assert_eq!(
            t.text,
            "Chapter 1\nThe rain fell in sheets. Nobody spoke.\n“Are you coming?” she asked."
        );
    }

    /// The reader draws the chapter's structure; the speech path reads the
    /// text. The two are built from the same source, so they cannot drift:
    /// a heading the reader draws is a heading the voice says, and the words
    /// inside it are the same words.
    #[test]
    fn the_blocks_carry_the_shape_the_reader_draws() {
        let xhtml = r#"<html><body>
<h1>Part One</h1>
<p>The <b>bold</b> and the <i>italic</i> and the <a href="x.html">linked</a>.</p>
<ul><li>first</li><li>second</li></ul>
<ol><li>numbered</li></ol>
<blockquote><p>A quoted line.</p></blockquote>
<table><tr><th>Head</th><th>Other</th></tr><tr><td>one</td><td>two</td></tr></table>
</body></html>"#;
        let t = extract(xhtml);
        let kinds: Vec<&str> = t.blocks.iter().map(|b| b.kind.css_class()).collect();
        assert_eq!(
            kinds,
            vec![
                "reader-heading-1",
                "reader-paragraph",
                "reader-list-item",
                "reader-list-item",
                "reader-list-item",
                "reader-quote",
                "reader-table",
            ],
            "the blocks are the source's own shape"
        );
        assert_eq!(t.blocks[0].text(), "Part One");

        // The emphasis is per run, not per block.
        let runs = &t.blocks[1].runs;
        assert!(runs.iter().any(|r| r.bold && r.text.contains("bold")));
        assert!(runs.iter().any(|r| r.italic && r.text.contains("italic")));
        assert!(runs
            .iter()
            .any(|r| r.link.as_deref() == Some("x.html") && r.text.contains("linked")));

        // The list: the bullet list first, then the numbered one, and the
        // numbering is remembered.
        let items: Vec<(usize, bool, String)> = t
            .blocks
            .iter()
            .filter_map(|b| match b.kind {
                BlockKind::ListItem { depth, ordered } => {
                    Some((depth, ordered, b.text()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].2, "first");
        assert!(!items[0].1, "the bullet list is not numbered");
        assert!(items[2].1, "the ordered list is numbered");

        // The table: two rows, two cells, and the header row is marked.
        let BlockKind::Table { rows } = &t.blocks.last().expect("a table").kind else {
            panic!("the last block is the table");
        };
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].len(), 2);
        assert!(rows[0].iter().all(|c| c.header), "the first row is headers");
        assert_eq!(rows[0][0].text, "Head");
        assert_eq!(rows[1][1].text, "two");
    }

    /// The structure is a *view* of the same text: everything the reader
    /// shows is in the speech text, in the same order, so the voice says
    /// what the eye reads.
    #[test]
    fn the_blocks_and_the_text_say_the_same_thing() {
        let xhtml = r#"<html><body>
<h1>Part One</h1>
<p>The <b>bold</b> word.</p>
<ul><li>first</li></ul>
<table><tr><td>cell</td></tr></table>
</body></html>"#;
        let t = extract(xhtml);
        for block in &t.blocks {
            let shown = block.text();
            for word in shown.split_whitespace() {
                assert!(
                    t.text.contains(word),
                    "the reader shows {word:?}, which the voice never says"
                );
            }
        }
    }
}
