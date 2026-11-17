//! HTML book reader — `.html`/`.htm` standalone files and `.htmlz`
//! archives (calibre's HTML container). The most common "ebook" format on
//! the web: every saved article, every Project Gutenberg HTML edition,
//! every site mirror someone keeps as a folder of files.
//!
//! HTML is NOT XML, and that single fact drives the design: a strict XML
//! parser aborts on the first `<br>`, unclosed `<p>` or unknown entity —
//! which in practice truncates the book at the top of chapter two. So this
//! parser is a hand-written tolerant tokenizer (ported from the iOS
//! HtmlBookParser.swift, which solved the same problem for the same
//! reasons):
//!
//! - void elements (`<br>`, `<img>`, `<hr>`) need no close;
//! - unclosed `<p>`/`<li>` are closed by the next block tag;
//! - `<script>`/`<style>` bodies and `<!-- … -->` comments are skipped
//!   whole, while `<head>` is deliberately walked (it carries the only
//!   `<title>` and author `<meta>` a saved page has);
//! - entities go through a named table plus numeric references; unknown
//!   names keep their whole `&name;` token verbatim rather than inventing
//!   a word;
//! - tags inside attribute values (the classic `alt="a > b"` bug) cannot
//!   confuse the scan, because attribute strings are consumed atomically.
//!
//! Chapters come from `<h1>`–`<h2>` (deeper levels stay inside as prose,
//! the same rule the MOBI and FB2 readers apply), and `<table>` becomes a
//! real table the chapter XHTML carries.

use super::bookdoc::{
    xml_escape, BookChapter, BookDoc, BookDocError, BookImage, BookToc,
};
use super::zip::Archive;

/// The office parsers' bound, so a heading-less HTML dump chunks the same
/// way a heading-less text file does.
const CHAPTER_BLOCK_LIMIT: usize = 150;

/// Parse a standalone `.html`/`.htm` document into chapters.
///
/// The images a standalone file references live outside the file — the
/// webview could not serve them, and a broken-image icon helps nobody —
/// so their tags are dropped. An HTMLZ keeps them; see
/// [`parse_htmlz`].
pub fn parse(data: &[u8]) -> Result<BookDoc, BookDocError> {
    parse_inner(data, false)
}

/// The parse, with the choice of whether image references survive.
fn parse_inner(data: &[u8], keep_images: bool) -> Result<BookDoc, BookDocError> {
    let text = decode(data);
    if text.trim().is_empty() {
        return Err(BookDocError::Malformed(
            "the file has no readable text".to_string(),
        ));
    }
    let mut reader = HtmlReader::new(&text);
    reader.run();
    if reader.items.is_empty() {
        return Err(BookDocError::Malformed(
            "no readable text in the HTML document".to_string(),
        ));
    }
    let title = reader.title.clone();
    let author = reader.author.clone();
    let (chapters, toc) = chapterize(reader.items, keep_images);
    Ok(BookDoc {
        title,
        author,
        chapters,
        toc,
        ..BookDoc::default()
    })
}

/// Parse a `.htmlz` — a ZIP with `index.html`, `metadata.opf` and the
/// images. The OPF supplies title/author exactly as an EPUB's does; the
/// images the HTML references get `img/` names and ship in the book.
pub fn parse_htmlz(data: &[u8]) -> Result<BookDoc, BookDocError> {
    let archive = Archive::parse(data.to_vec())
        .map_err(|e| BookDocError::Malformed(format!("not an HTML archive: {e}")))?;
    let names: Vec<String> = archive.entries().iter().map(|e| e.name.clone()).collect();
    let html_name = names
        .iter()
        .find(|n| n.as_str() == "index.html")
        .or_else(|| {
            names
                .iter()
                .find(|n| n.to_lowercase().ends_with(".html") && !n.to_lowercase().ends_with(".htmlz"))
        })
        .ok_or_else(|| BookDocError::Malformed("the archive has no index.html".to_string()))?;
    let html_data = archive
        .read(html_name)
        .map_err(|e| BookDocError::Malformed(format!("{html_name}: {e}")))?;
    let mut doc = parse_inner(&html_data, true)?;

    // OPF metadata: Dublin-Core title/creator, the same walk the EPUB
    // parser makes over its OPF. The OPF is the book's own metadata and
    // wins over the HTML page's <title> — calibre writes "Book Title" in
    // the OPF and the file name of the converted page in the <title>.
    if let Some(opf_name) = names.iter().find(|n| n.to_lowercase().ends_with(".opf")) {
        if let Ok(opf) = archive.read_string(opf_name) {
            let (title, author) = opf_metadata(&opf);
            if title.is_some() {
                doc.title = title;
            }
            if author.is_some() {
                doc.author = author;
            }
        }
    }

    // Cover: the first image entry, exactly the convention calibre writes
    // (cover.jpg/cover.png beside index.html).
    for name in &names {
        let lower = name.to_lowercase();
        let is_image = lower.ends_with(".jpg") || lower.ends_with(".jpeg") || lower.ends_with(".png");
        if !is_image {
            continue;
        }
        if let Ok(cover_data) = archive.read(name) {
            if !cover_data.is_empty() {
                doc.cover = Some(BookImage {
                    name: "img/cover".to_string(),
                    mime: if lower.ends_with(".png") {
                        "image/png".to_string()
                    } else {
                        "image/jpeg".to_string()
                    },
                    data: cover_data,
                });
                break;
            }
        }
    }

    // The images the HTML references: every `<img src>` that resolves to an
    // entry ships as `img/<entry name>`, and the chapter XHTML's srcs are
    // rewritten to those names. An src the archive does not hold (an
    // absolute URL, a file the mirror lost) loses its tag — a broken image
    // the webview would draw as a missing-file icon helps nobody.
    rewrite_htmlz_images(&mut doc, &archive, &names);
    Ok(doc)
}

/// Rewrite every chapter's `<img src="…">` that resolves to a zip entry
/// into `<img src="img/…">`, collecting the entries as the book's images.
fn rewrite_htmlz_images(doc: &mut BookDoc, archive: &Archive, names: &[String]) {
    for chapter in &mut doc.chapters {
        let mut html = String::with_capacity(chapter.html.len());
        let mut rest = chapter.html.as_str();
        while let Some(at) = rest.find("<img") {
            html.push_str(&rest[..at]);
            let after = &rest[at..];
            let Some(tag_end) = after.find('>') else {
                html.push_str(after);
                rest = "";
                break;
            };
            let (tag, next) = after.split_at(tag_end + 1);
            if let Some(new_tag) = resolve_img_tag(tag, archive, names, &mut doc.images) {
                html.push_str(&new_tag);
            }
            rest = next;
        }
        html.push_str(rest);
        chapter.html = html;
    }
}

/// One `<img …>` tag: when its src names an entry in the archive, return
/// the tag rewritten to the `img/` name (adding the entry to `images`);
/// otherwise `None` — the tag is dropped.
fn resolve_img_tag(
    tag: &str,
    archive: &Archive,
    names: &[String],
    images: &mut Vec<BookImage>,
) -> Option<String> {
    let src_at = tag.to_lowercase().find("src=")?;
    let rest = &tag[src_at + 4..];
    let quote = rest.chars().next()?;
    let src = if quote == '"' || quote == '\'' {
        let end = rest[1..].find(quote)? + 1;
        &rest[1..end]
    } else {
        rest.split(|c: char| c.is_whitespace() || c == '>').next()?
    };
    let src = super::epub::percent_decode(src);
    // Exact entry name, then entry-basename — htmlz mirrors vary in how
    // much of the original site's directory tree they keep.
    let entry = names
        .iter()
        .find(|n| n.as_str() == src)
        .or_else(|| {
            let base = src.rsplit('/').next()?;
            names.iter().find(|n| {
                n.rsplit('/').next().is_some_and(|tail| tail == base)
                    && (base.ends_with(".jpg")
                        || base.ends_with(".jpeg")
                        || base.ends_with(".png")
                        || base.ends_with(".gif"))
            })
        })?;
    let data = archive.read(entry).ok()?;
    // The book's image names are flat — the file name alone — because the
    // reader serves them under one virtual directory and a path with
    // slashes in it would need escaping in every reference.
    let base = entry.rsplit('/').next().unwrap_or(entry);
    let image_name = unique_name(base, images);
    let mime = if entry.to_lowercase().ends_with(".png") {
        "image/png"
    } else if entry.to_lowercase().ends_with(".gif") {
        "image/gif"
    } else {
        "image/jpeg"
    };
    images.push(BookImage {
        name: format!("img/{image_name}"),
        mime: mime.to_string(),
        data,
    });
    Some(format!(
        "<img src=\"img/{image_name}\" alt=\"{}\">",
        alt_of(tag).unwrap_or_default()
    ))
}

/// A name no other image in the book holds: `pic.png`, then `pic-2.png`,
/// `pic-3.png`. Two mirrors may carry the same file name in different
/// directories, and both pictures belong in the book.
fn unique_name(base: &str, images: &[BookImage]) -> String {
    let taken = |name: &str| images.iter().any(|image| image.name == format!("img/{name}"));
    if !taken(base) {
        return base.to_string();
    }
    let (stem, extension) = match base.rsplit_once('.') {
        Some((stem, extension)) => (stem, format!(".{extension}")),
        None => (base, String::new()),
    };
    for n in 2..1000 {
        let candidate = format!("{stem}-{n}{extension}");
        if !taken(&candidate) {
            return candidate;
        }
    }
    format!("{stem}-{}", images.len())
}

/// An `<img>` tag's alt text, if it carried one.
fn alt_of(tag: &str) -> Option<String> {
    let lower = tag.to_lowercase();
    let at = lower.find("alt=")?;
    let rest = &tag[at + 4..];
    let quote = rest.chars().next()?;
    if quote == '"' || quote == '\'' {
        let end = rest[1..].find(quote)? + 1;
        Some(rest[1..end].to_string())
    } else {
        Some(rest.split(|c: char| c.is_whitespace() || c == '>').next()?.to_string())
    }
}

/// Title/creator out of an HTMLZ's `metadata.opf` — Dublin-Core, the same
/// walk the EPUB parser makes.
fn opf_metadata(opf: &str) -> (Option<String>, Option<String>) {
    let mut title: Option<String> = None;
    let mut creator: Option<String> = None;
    let mut current: Option<String> = None;
    let mut buffer = String::new();
    let mut p = super::xml::Parser::new(opf);
    while let Ok(Some(event)) = p.next_event() {
        match event {
            super::xml::Event::Start { name, .. } | super::xml::Event::Empty { name, .. } => {
                let local = local_name(&name);
                current = match local.as_str() {
                    "title" | "creator" if title.is_none() || creator.is_none() => {
                        buffer.clear();
                        Some(local)
                    }
                    _ => None,
                };
            }
            super::xml::Event::Text(text) => {
                if current.is_some() {
                    buffer.push_str(&text);
                }
            }
            super::xml::Event::End { name } => {
                if current.as_deref() == Some(local_name(&name).as_str()) {
                    let value = buffer.trim().to_string();
                    if !value.is_empty() {
                        if local_name(&name) == "title" && title.is_none() {
                            title = Some(value);
                        } else if local_name(&name) == "creator" && creator.is_none() {
                            creator = Some(value);
                        }
                    }
                    current = None;
                }
            }
            _ => {}
        }
    }
    (title, creator)
}

fn local_name(name: &str) -> String {
    name.rsplit(':').next().unwrap_or(name).to_string()
}

/// Browsers sniff; we can't. A UTF-16 BOM first (it is the only
/// unambiguous signal), then UTF-8 (validates), then a meta charset naming
/// UTF-16, then Windows-1252, then latin-1 — the same floor as the
/// plain-text battery, and the same reasoning: latin-1 never fails.
pub fn decode(data: &[u8]) -> String {
    if let Some(text) = decode_utf16_bom(data) {
        return text;
    }
    if let Ok(text) = std::str::from_utf8(data) {
        return text.to_string();
    }
    let head = String::from_utf8_lossy(&data[..data.len().min(2048)]).to_lowercase();
    if head.contains("charset=utf-16") {
        // No BOM, but the document says UTF-16. The declaration is evidence
        // enough for little-endian, the x86 norm.
        return decode_utf16(data, false);
    }
    super::bookdoc::decode_cp1252(data)
}

/// UTF-16 text, when the bytes carry a byte-order mark.
fn decode_utf16_bom(data: &[u8]) -> Option<String> {
    if data.starts_with(&[0xFE, 0xFF]) {
        Some(decode_utf16(&data[2..], true))
    } else if data.starts_with(&[0xFF, 0xFE]) {
        Some(decode_utf16(&data[2..], false))
    } else {
        None
    }
}

/// UTF-16 units → text. An odd trailing byte is dropped: the half a
/// character lost is not worth a parse failure.
fn decode_utf16(data: &[u8], big_endian: bool) -> String {
    let units: Vec<u16> = data
        .chunks_exact(2)
        .map(|pair| {
            if big_endian {
                u16::from_be_bytes([pair[0], pair[1]])
            } else {
                u16::from_le_bytes([pair[0], pair[1]])
            }
        })
        .collect();
    String::from_utf16_lossy(&units)
}

// ---------------------------------------------------------------------------
// The tolerant reader
// ---------------------------------------------------------------------------

/// One table cell: its text and whether it was a header cell.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cell {
    text: String,
    header: bool,
}

/// One thing the tokenizer found, in document order.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Item {
    Heading { text: String, level: usize },
    Paragraph(String),
    Table(Vec<Vec<Cell>>),
    /// An image reference. A standalone HTML file's images live outside
    /// the file and are dropped; an HTMLZ's images live in the archive and
    /// are rewritten to the book's own `img/` names.
    Image { src: String, alt: Option<String> },
}

/// Hand-written HTML tokenizer → heading/paragraph/table items.
///
/// The tokenizer's only lookahead mechanism is a one-character pushback
/// stack: `peek` never consumes, and `pushback` hands a character back
/// when the scan has to reconsider it (an entity that turned out not to
/// be one, an unquoted attribute value's terminator).
struct HtmlReader {
    source: std::vec::IntoIter<char>,
    /// Characters handed back by the scanner, last one first.
    pushback: Vec<char>,
    items: Vec<Item>,
    title: Option<String>,
    author: Option<String>,
    buffer: String,
    pending_heading_level: usize,
    table_rows: Vec<Vec<Cell>>,
    row_cells: Vec<Cell>,
    cell_buffer: String,
    in_cell: bool,
    in_header_cell: bool,
    in_title: bool,
    title_buffer: String,
    /// Inside a `<pre>` block: the source's newlines are the author's line
    /// breaks, not formatting to collapse. Project Gutenberg's Australian
    /// editions put whole sections of front matter in `<pre>`; without this
    /// flag every line of it tidy()s into ONE paragraph and the reader gets
    /// a wall of text.
    in_pre: bool,
}

impl HtmlReader {
    fn new(text: &str) -> Self {
        Self {
            source: text.chars().collect::<Vec<char>>().into_iter(),
            pushback: Vec::new(),
            items: Vec::new(),
            title: None,
            author: None,
            buffer: String::new(),
            pending_heading_level: 0,
            table_rows: Vec::new(),
            row_cells: Vec::new(),
            cell_buffer: String::new(),
            in_cell: false,
            in_header_cell: false,
            in_title: false,
            title_buffer: String::new(),
            in_pre: false,
        }
    }

    /// The next character without consuming it.
    fn peek(&self) -> Option<char> {
        self.pushback
            .last()
            .copied()
            .or_else(|| self.source.as_slice().first().copied())
    }

    /// The next character, consuming it.
    fn next_char(&mut self) -> Option<char> {
        self.pushback.pop().or_else(|| self.source.next())
    }

    /// Hand a character back to the scanner.
    fn push_back(&mut self, c: char) {
        self.pushback.push(c);
    }

    fn run(&mut self) {
        while let Some(c) = self.next_char() {
            if c == '<' {
                self.flush_text();
                self.read_tag();
                continue;
            }
            if c == '&' {
                // `read_entity` consumes the entity's own terminator; the
                // scanner must not eat a second character here (that
                // double-consume ate the space after every entity, which
                // ran the words of a sentence together).
                let entity = self.read_entity();
                self.append_literal(&entity);
                continue;
            }
            self.append_literal_char(c);
        }
        self.flush_text();
    }

    /// One run of document text. While a `<title>` is open it goes to the
    /// title buffer instead of the paragraph buffer — without this the
    /// title never accumulated at all, which is why every HTML book came
    /// in with a blank title.
    fn append_literal(&mut self, value: &str) {
        if self.in_title {
            self.title_buffer.push_str(value);
        } else {
            self.buffer.push_str(value);
        }
    }

    fn append_literal_char(&mut self, value: char) {
        if self.in_title {
            self.title_buffer.push(value);
        } else {
            self.buffer.push(value);
        }
    }

    /// Consumes one tag (and, for script/style/comment, its whole body).
    /// The open `<` was already consumed; the lookahead char is the one
    /// after it.
    fn read_tag(&mut self) {
        let Some(first) = self.peek() else {
            return;
        };
        // Comment: `<!--` skips to `-->`.
        if first == '!' {
            self.next_char();
            let second = self.peek();
            let third = self.second_after_pushback();
            let is_comment = second == Some('-') && third == Some('-');
            if is_comment {
                self.next_char();
                self.next_char();
                self.skip_to_comment_end();
            } else {
                // `<!DOCTYPE …>` and other declarations: to the '>'.
                self.skip_to('>');
            }
            return;
        }
        let mut name = String::new();
        let mut is_closing = false;
        if first == '/' {
            is_closing = true;
            self.next_char();
        }
        while let Some(c) = self.next_char() {
            if c == '>' {
                self.handle(&name, is_closing, &[]);
                return;
            }
            if c.is_whitespace() {
                break;
            }
            name.extend(c.to_lowercase());
        }
        // Attributes: consume to the closing '>', honouring quoted strings
        // so an `alt="a > b"` cannot end the tag early.
        let mut attributes: Vec<(String, String)> = Vec::new();
        loop {
            let Some(mut c) = self.peek() else { break };
            if c == '>' {
                break;
            }
            let mut attribute_name = String::new();
            while let Some(n) = self.next_char() {
                if n == '=' || n == '>' || n.is_whitespace() {
                    c = n;
                    break;
                }
                attribute_name.extend(n.to_lowercase());
            }
            if c == '=' {
                // The '=' was already consumed by the scan above — reading
                // one more character here ate the value's opening quote,
                // which turned every attribute value into the rest of the
                // tag (and lost the meta author the shelf shows).
                let mut value = String::new();
                if let Some(quote) = self.peek() {
                    if quote == '"' || quote == '\'' {
                        self.next_char();
                        while let Some(v) = self.next_char() {
                            if v == quote {
                                break;
                            }
                            value.push(v);
                        }
                    } else {
                        while let Some(v) = self.next_char() {
                            if v == '>' || v.is_whitespace() {
                                // Unquoted value: leave the terminator for
                                // the attribute loop.
                                self.push_back(v);
                                break;
                            }
                            value.push(v);
                        }
                    }
                }
                if !attribute_name.is_empty() {
                    attributes.push((attribute_name, value));
                }
            } else if c == '>' {
                break;
            } else {
                // A bare attribute name with no value; the terminator stays.
                if !attribute_name.is_empty() {
                    attributes.push((attribute_name, String::new()));
                }
            }
        }
        // Past the '>'.
        if self.peek() == Some('>') {
            self.next_char();
        }
        self.handle(&name, is_closing, &attributes);
    }

    /// The character *after* the one `peek` would return — the second '-'
    /// of a `<!--` comment opener.
    fn second_after_pushback(&self) -> Option<char> {
        if !self.pushback.is_empty() {
            self.pushback
                .iter()
                .rev()
                .nth(1)
                .copied()
                .or_else(|| self.source.as_slice().first().copied())
        } else {
            self.source.as_slice().get(1).copied()
        }
    }

    fn skip_to(&mut self, terminator: char) {
        while let Some(c) = self.next_char() {
            if c == terminator {
                return;
            }
        }
    }

    fn skip_to_comment_end(&mut self) {
        let mut last_two = (None, None);
        while let Some(c) = self.next_char() {
            if last_two == (Some('-'), Some('-')) && c == '>' {
                return;
            }
            last_two = (last_two.1, Some(c));
        }
    }

    /// Skips the whole body of `<script>`/`<style>` to the matching close,
    /// honouring nesting. `read_tag` has consumed the OPENING tag already,
    /// so depth starts at ONE — the first `</script>` closes it.
    fn skip_until_close(&mut self, tag: &str) {
        let mut depth = 1usize;
        while let Some(c) = self.next_char() {
            if c != '<' {
                continue;
            }
            let mut name = String::new();
            let mut is_closing = false;
            let mut cursor_tag = String::new();
            if self.peek() == Some('/') {
                is_closing = true;
                self.next_char();
            }
            while let Some(n) = self.next_char() {
                if n == '>' || n.is_whitespace() {
                    cursor_tag = name.clone();
                    break;
                }
                name.extend(n.to_lowercase());
            }
            if cursor_tag != tag {
                continue;
            }
            if is_closing {
                depth -= 1;
                if depth == 0 {
                    return;
                }
            } else {
                depth += 1;
            }
        }
    }

    /// One named entity or numeric reference, decoded. Unknown names keep
    /// their whole `&weirdname;` token — the source's own stray markup, not
    /// an invented word.
    fn read_entity(&mut self) -> String {
        let mut name = String::new();
        // The characters read so far, so a failed entity can hand them
        // back: `&foo` without the ';' is literal text, not a name the
        // reader may swallow.
        let mut consumed: Vec<char> = Vec::new();
        while name.chars().count() < 32 {
            let Some(c) = self.peek() else { break };
            if c == ';' {
                break;
            }
            if !(c.is_alphanumeric() || c == '#') {
                break;
            }
            name.push(c);
            consumed.push(c);
            self.next_char();
        }
        if self.peek() != Some(';') || name.is_empty() {
            for c in consumed.into_iter().rev() {
                self.push_back(c);
            }
            return "&".to_string();
        }
        self.next_char(); // the ';'
        if let Some(digits) = name.strip_prefix('#') {
            let scalar = if let Some(hex) = digits.strip_prefix(['x', 'X']) {
                u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
            } else {
                digits.parse::<u32>().ok().and_then(char::from_u32)
            };
            return scalar.map(String::from).unwrap_or_default();
        }
        match named_entity(&name) {
            Some(decoded) => decoded.to_string(),
            None => format!("&{name};"),
        }
    }

    // -- semantics ---------------------------------------------------------

    fn handle(&mut self, tag: &str, is_closing: bool, attributes: &[(String, String)]) {
        match tag {
            "script" | "style" => {
                // The whole body is unreadable noise for a reader.
                // `<head>` is deliberately NOT skipped: it carries the only
                // `<title>` and author `<meta>` a saved page has.
                if !is_closing {
                    self.flush_text();
                    self.skip_until_close(tag);
                }
            }
            "head" => {}
            "title" => {
                if is_closing {
                    let value = tidy(&self.title_buffer);
                    if !value.is_empty() && self.title.is_none() {
                        self.title = Some(value);
                    }
                    self.in_title = false;
                } else {
                    self.in_title = true;
                    self.title_buffer.clear();
                }
            }
            "meta" => {
                let name = attributes
                    .iter()
                    .find(|(n, _)| n == "name" || n == "property")
                    .map(|(_, v)| v.to_lowercase())
                    .unwrap_or_default();
                let content = attributes
                    .iter()
                    .find(|(n, _)| n == "content")
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default();
                if name == "author" || name == "citation_author" || name == "dc.creator" {
                    let value = tidy(&content);
                    if !value.is_empty() && self.author.is_none() {
                        self.author = Some(value);
                    }
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = tag[1..].parse().unwrap_or(6);
                if is_closing {
                    self.flush_text();
                    self.pending_heading_level = 0;
                } else {
                    self.flush_text();
                    self.pending_heading_level = level;
                }
            }
            "p" | "div" | "li" | "tr" | "blockquote" | "section" | "article" | "dd" | "dt"
            | "figcaption" | "ul" | "ol" => {
                if is_closing {
                    self.close_block(false);
                }
            }
            // Preformatted text: the line breaks are the book's own. The
            // block's lines become one paragraph each — a reader wants the
            // Gutenberg front matter readable, not one tidied wall.
            "pre" => {
                if is_closing {
                    self.flush_pre();
                    self.in_pre = false;
                } else {
                    self.flush_text();
                    self.in_pre = true;
                }
            }
            "table" => {
                if is_closing {
                    self.close_block(true);
                }
            }
            "br" | "hr" => {
                self.flush_text();
            }
            "img" => {
                // An image is a boundary even without a close tag — an
                // alt-less image must not glue two words together.
                self.flush_text();
                if !is_closing {
                    let src = attributes
                        .iter()
                        .find(|(n, _)| n == "src")
                        .map(|(_, v)| v.clone())
                        .unwrap_or_default();
                    if !src.is_empty() {
                        let alt = attributes
                            .iter()
                            .find(|(n, _)| n == "alt")
                            .map(|(_, v)| v.clone())
                            .filter(|v| !v.is_empty());
                        self.items.push(Item::Image { src, alt });
                    }
                }
            }
            "td" | "th" => {
                if is_closing {
                    if self.in_cell {
                        let text = tidy(&self.cell_buffer);
                        self.row_cells.push(Cell {
                            text,
                            header: self.in_header_cell,
                        });
                        self.cell_buffer.clear();
                        self.in_cell = false;
                        self.in_header_cell = false;
                    }
                } else {
                    self.in_cell = true;
                    self.in_header_cell = tag == "th";
                    self.cell_buffer.clear();
                }
            }
            _ => {}
        }
    }

    /// Text since the last boundary becomes a paragraph — or the heading's
    /// title when a heading is pending. Inside a `<pre>` the boundary emits
    /// the buffer's lines whole (see [`Self::flush_pre`]) and the pre block
    /// stays open.
    fn flush_text(&mut self) {
        if self.in_pre {
            self.flush_pre();
            return;
        }
        let text = tidy(&self.buffer);
        self.buffer.clear();
        if text.is_empty() {
            return;
        }
        if self.pending_heading_level > 0 {
            self.items.push(Item::Heading {
                text,
                level: self.pending_heading_level,
            });
            self.pending_heading_level = 0;
        } else if self.in_cell {
            if !self.cell_buffer.is_empty() {
                self.cell_buffer.push(' ');
            }
            self.cell_buffer.push_str(&text);
        } else {
            self.items.push(Item::Paragraph(text));
        }
    }

    /// The `<pre>` buffer's lines, as paragraphs. One line each: the source
    /// formatted these lines by hand (a Gutenberg header, a poem's stanza),
    /// and each line is a unit the reader's own wrapping can lay out. An
    /// empty line is a blank line the author typed — it becomes nothing, so
    /// the paragraphs carry the text, not the spacing.
    fn flush_pre(&mut self) {
        let text = std::mem::take(&mut self.buffer);
        for line in text.split('\n') {
            let line = tidy(line);
            if line.is_empty() {
                continue;
            }
            self.items.push(Item::Paragraph(line));
        }
    }

    /// A closing block tag: text first, then the cell/row bookkeeping.
    /// `<tr>` closes a ROW and `</table>` closes the TABLE — two different
    /// boundaries; conflating them meant every table came out as run-on
    /// paragraphs ("NameQty", "Apples12").
    fn close_block(&mut self, is_table: bool) {
        self.flush_text();
        if self.in_cell {
            let text = tidy(&self.cell_buffer);
            self.row_cells.push(Cell {
                text,
                header: self.in_header_cell,
            });
            self.cell_buffer.clear();
            self.in_cell = false;
            self.in_header_cell = false;
        }
        if !self.row_cells.is_empty() {
            self.table_rows.push(std::mem::take(&mut self.row_cells));
        }
        if is_table && !self.table_rows.is_empty() {
            self.items
                .push(Item::Table(std::mem::take(&mut self.table_rows)));
        }
    }
}

/// The named entities the reader decodes, beyond `xml::decode_entities`'s
/// set: the Latin-1 supplement gaps, the accented letters older books
/// spell with entities, and the Greek letters a scientific book reaches
/// for. Unknown names keep their whole `&name;` token.
fn named_entity(name: &str) -> Option<&'static str> {
    let entity = match name {
        // XML's five.
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        // The HTML5 set XHTML books use.
        "nbsp" => "\u{00A0}",
        "hellip" => "\u{2026}",
        "mdash" => "\u{2014}",
        "ndash" => "\u{2013}",
        "lsquo" => "\u{2018}",
        "rsquo" => "\u{2019}",
        "ldquo" => "\u{201C}",
        "rdquo" => "\u{201D}",
        "bull" => "\u{2022}",
        "dagger" => "\u{2020}",
        "permil" => "\u{2030}",
        "lsaquo" => "\u{2039}",
        "rsaquo" => "\u{203A}",
        "copy" => "\u{00A9}",
        "reg" => "\u{00AE}",
        "trade" => "\u{2122}",
        "deg" => "\u{00B0}",
        "plusmn" => "\u{00B1}",
        "middot" => "\u{00B7}",
        "para" => "\u{00B6}",
        "sect" => "\u{00A7}",
        "frac12" => "\u{00BD}",
        "frac14" => "\u{00BC}",
        "frac34" => "\u{00BE}",
        "times" => "\u{00D7}",
        "divide" => "\u{00F7}",
        // Accented letters — the ones prose actually reaches for.
        "agrave" => "\u{00E0}", "aacute" => "\u{00E1}", "acirc" => "\u{00E2}",
        "atilde" => "\u{00E3}", "auml" => "\u{00E4}", "aring" => "\u{00E5}",
        "aelig" => "\u{00E6}", "ccedil" => "\u{00E7}", "egrave" => "\u{00E8}",
        "eacute" => "\u{00E9}", "ecirc" => "\u{00EA}", "euml" => "\u{00EB}",
        "igrave" => "\u{00EC}", "iacute" => "\u{00ED}", "icirc" => "\u{00EE}",
        "iuml" => "\u{00EF}", "ntilde" => "\u{00F1}", "ograve" => "\u{00F2}",
        "oacute" => "\u{00F3}", "ocirc" => "\u{00F4}", "otilde" => "\u{00F5}",
        "ouml" => "\u{00F6}", "oslash" => "\u{00F8}", "ugrave" => "\u{00F9}",
        "uacute" => "\u{00FA}", "ucirc" => "\u{00FB}", "uuml" => "\u{00FC}",
        "yacute" => "\u{00FD}", "yuml" => "\u{00FF}",
        "Agrave" => "\u{00C0}", "Aacute" => "\u{00C1}", "Acirc" => "\u{00C2}",
        "Atilde" => "\u{00C3}", "Auml" => "\u{00C4}", "Aring" => "\u{00C5}",
        "AElig" => "\u{00C6}", "Ccedil" => "\u{00C7}", "Egrave" => "\u{00C8}",
        "Eacute" => "\u{00C9}", "Ecirc" => "\u{00CA}", "Euml" => "\u{00CB}",
        "Igrave" => "\u{00CC}", "Iacute" => "\u{00CD}", "Icirc" => "\u{00CE}",
        "Iuml" => "\u{00CF}", "Ntilde" => "\u{00D1}", "Ograve" => "\u{00D2}",
        "Oacute" => "\u{00D3}", "Ocirc" => "\u{00D4}", "Otilde" => "\u{00D5}",
        "Ouml" => "\u{00D6}", "Oslash" => "\u{00D8}", "Ugrave" => "\u{00D9}",
        "Uacute" => "\u{00DA}", "Ucirc" => "\u{00DB}", "Uuml" => "\u{00DC}",
        "Yacute" => "\u{00DD}", "szlig" => "\u{00DF}",
        // Greek.
        "alpha" => "\u{03B1}", "beta" => "\u{03B2}", "gamma" => "\u{03B3}",
        "delta" => "\u{03B4}", "pi" => "\u{03C0}", "mu" => "\u{03BC}",
        "sigma" => "\u{03C3}", "omega" => "\u{03C9}",
        _ => return None,
    };
    Some(entity)
}

/// Collapse the whitespace the source's own formatting carried — newlines
/// inside a paragraph, runs of indentation — into the single spaces prose
/// has.
fn tidy(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_space = false;
    for c in text.chars() {
        if c.is_whitespace() {
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

// ---------------------------------------------------------------------------
// Chapters
// ---------------------------------------------------------------------------

/// Splits the items into chapters at their `h1`/`h2` headings; deeper
/// headings stay inside as prose. Returns the chapters and the TOC their
/// headings describe.
///
/// `keep_images` decides whether an `<img>` becomes a tag in the chapter
/// XHTML: yes for an archive whose images travel with the book, no for a
/// standalone page whose images live somewhere the reader cannot reach.
fn chapterize(items: Vec<Item>, keep_images: bool) -> (Vec<BookChapter>, Vec<BookToc>) {
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
    ) {
        if !html.is_empty() || title.is_some() {
            let label = title.clone().unwrap_or_else(|| {
                format!("Chapter {}", chapters.len() + 1)
            });
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
    }

    for item in items {
        match item {
            Item::Heading { text, level } => {
                if level <= 2 || title.is_none() {
                    flush(&mut chapters, &mut toc, &mut title, &mut html);
                    title = Some(text);
                } else {
                    html.push_str("<p>");
                    html.push_str(&xml_escape(&text));
                    html.push_str("</p>");
                    paragraph_count += 1;
                }
            }
            Item::Paragraph(text) => {
                html.push_str("<p>");
                html.push_str(&xml_escape(&text));
                html.push_str("</p>");
                paragraph_count += 1;
            }
            Item::Table(rows) => {
                html.push_str("<table>");
                for row in rows {
                    html.push_str("<tr>");
                    for cell in row {
                        if cell.header {
                            html.push_str("<th>");
                        } else {
                            html.push_str("<td>");
                        }
                        html.push_str(&xml_escape(&cell.text));
                        if cell.header {
                            html.push_str("</th>");
                        } else {
                            html.push_str("</td>");
                        }
                    }
                    html.push_str("</tr>");
                }
                html.push_str("</table>");
            }
            Item::Image { src, alt } => {
                if keep_images {
                    html.push_str("<img src=\"");
                    html.push_str(&xml_escape(&src));
                    html.push_str("\"");
                    if let Some(alt) = alt {
                        html.push_str(" alt=\"");
                        html.push_str(&xml_escape(&alt));
                        html.push_str("\"");
                    }
                    html.push_str(">");
                }
            }
        }
        // The bound: a heading-less dump chunks like the text reader does,
        // and the section title carries onto the continuation chapter so
        // the contents list never shows "Chapter N" mid-section.
        if paragraph_count >= CHAPTER_BLOCK_LIMIT {
            let carried = title.take();
            flush(&mut chapters, &mut toc, &mut title, &mut html);
            title = carried;
            paragraph_count = 0;
        }
    }
    flush(&mut chapters, &mut toc, &mut title, &mut html);

    (chapters, toc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gutenberg_style_page_chapters_on_h2() {
        let html = "<html><head><title>An Essay</title></head><body>\
            <h1>An Essay</h1><p>Front matter prose.</p>\
            <h2>Chapter One</h2><p>Prose one.</p>\
            <h2>Chapter Two</h2><p>Prose two.</p></body></html>";
        let doc = parse(html.as_bytes()).unwrap();
        assert_eq!(doc.title.as_deref(), Some("An Essay"));
        assert_eq!(doc.chapters.len(), 3);
        assert_eq!(doc.chapters[0].title.as_deref(), Some("An Essay"));
        assert_eq!(doc.chapters[1].title.as_deref(), Some("Chapter One"));
        assert!(doc.chapters[1].html.contains("<p>Prose one.</p>"));
        assert_eq!(doc.toc[1].label, "Chapter One");
        assert_eq!(doc.toc[1].chapter_index, 1);
    }

    #[test]
    fn void_elements_and_unclosed_paragraphs_do_not_truncate() {
        let html = "<body><p>one<br>line<p>two<img src=\"x.png\"><p>three";
        let doc = parse(html.as_bytes()).unwrap();
        let text = doc.chapters.iter().map(|c| c.html.as_str()).collect::<String>();
        assert!(text.contains("one"), "{text}");
        assert!(text.contains("two"), "{text}");
        assert!(text.contains("three"), "{text}");
        // The external image loses its tag — nothing to serve it from.
        assert!(!text.contains("<img"), "{text}");
    }

    #[test]
    fn attributes_with_gt_inside_quotes_survive() {
        let html = "<p title=\"a > b\">after the alt bug</p>";
        let doc = parse(html.as_bytes()).unwrap();
        assert!(doc.chapters[0].html.contains("after the alt bug"));
    }

    #[test]
    fn script_and_style_bodies_are_skipped_whole() {
        let html = "<head><style>p { color: red; }</style><script>if (a<b) x();</script>\
            <title>Kept</title></head><body><p>prose</p></body>";
        let doc = parse(html.as_bytes()).unwrap();
        assert_eq!(doc.title.as_deref(), Some("Kept"));
        let text = &doc.chapters[0].html;
        assert!(text.contains("prose"), "{text}");
        assert!(!text.contains("color"), "{text}");
        assert!(!text.contains("x()"), "{text}");
    }

    #[test]
    fn comments_and_doctype_are_skipped() {
        let html = "<!DOCTYPE html><!-- a note <p>hidden</p> --><p>shown</p>";
        let doc = parse(html.as_bytes()).unwrap();
        let text = doc.chapters.iter().map(|c| c.html.as_str()).collect::<String>();
        assert!(text.contains("shown"), "{text}");
        assert!(!text.contains("hidden"), "{text}");
    }

    #[test]
    fn entities_decode_and_unknown_names_survive() {
        let html = "<p>Caf&eacute; &mdash; na&iuml;ve & &weirdname; &#x41;&#66;</p>";
        let doc = parse(html.as_bytes()).unwrap();
        let text = &doc.chapters[0].html;
        assert!(text.contains("Café"), "{text}");
        assert!(text.contains("—"), "{text}");
        assert!(text.contains("naïve"), "{text}");
        // The chapter XHTML is real markup: a decoded '&' is written back
        // escaped, and an unknown name survives as its own token — the
        // reader shows "&weirdname;", it does not swallow the word after
        // it, and the speech pass says the same.
        assert!(text.contains("&"), "{text}");
        // In the XHTML the '&' is escaped — that is what makes the token
        // survive a real XML/HTML parser instead of being read as markup.
        // The token survives whole; only its '&' is escaped for the XHTML.
        assert!(text.contains("weirdname;"), "{text}");
        assert!(text.contains("AB"), "{text}");
        let spoken = super::super::chapter::extract(&doc.chapters[0].html).text;
        assert!(spoken.contains("&weirdname;"), "{spoken}");
    }

    #[test]
    fn an_entity_without_its_terminator_stays_text() {
        let html = "<p>AT&T and a bare &amp word</p>";
        let doc = parse(html.as_bytes()).unwrap();
        let text = &doc.chapters[0].html;
        // The bare '&amp' (no ';') is literal text, not a name the reader
        // may swallow: the word after it must survive, and the '&' in
        // "AT&T" must not eat the "T".
        assert!(text.contains("AT"), "{text}");
        assert!(text.contains("T and a bare"), "{text}");
        assert!(text.contains("amp word"), "{text}");
    }


    #[test]
    fn tables_become_table_markup_with_headers() {
        let html = "<table><tr><th>Name</th><th>Qty</th></tr><tr><td>Apples</td><td>12</td></tr></table>";
        let doc = parse(html.as_bytes()).unwrap();
        let text = &doc.chapters[0].html;
        assert!(text.contains("<table>"), "{text}");
        assert!(text.contains("<th>Name</th>"), "{text}");
        assert!(text.contains("<td>Apples</td>"), "{text}");
        assert!(text.contains("<td>12</td>"), "{text}");
    }

    #[test]
    fn meta_author_is_found() {
        let html = "<head><meta name=\"author\" content=\"Jane A. Doe\"></head><body><p>x</p></body>";
        let doc = parse(html.as_bytes()).unwrap();
        assert_eq!(doc.author.as_deref(), Some("Jane A. Doe"));
    }

    #[test]
    fn deep_headings_stay_inside_their_chapter() {
        let html = "<h1>Part</h1><p>a.</p><h3>Section</h3><p>b.</p>";
        let doc = parse(html.as_bytes()).unwrap();
        assert_eq!(doc.chapters.len(), 1);
        assert!(doc.chapters[0].html.contains("<p>Section</p>"));
    }

    #[test]
    fn empty_input_is_refused() {
        let err = parse(b"   ").unwrap_err();
        assert!(err.to_string().contains("no readable text"), "{err}");
        let err = parse(b"<html><body></body></html>").unwrap_err();
        assert!(err.to_string().contains("no readable text"), "{err}");
    }

    /// The Gutenberg front-matter shape: the header sits in a `<pre>` block
    /// whose newlines are the layout. Without the pre handling, tidy()
    /// collapsed every line into ONE paragraph — a wall of text with the
    /// title, the eBook number and the license run together.
    #[test]
    fn a_pre_block_reads_one_line_per_paragraph() {
        let html = "<html><body><pre>Title:      The Great Gatsby\n\
                    Author:     F. Scott Fitzgerald\n\n\
                    * A Project Gutenberg of Australia eBook *</pre>\
                    <p>prose after the pre</p></body></html>";
        let doc = parse(html.as_bytes()).unwrap();
        let items_text: Vec<String> = doc
            .chapters
            .iter()
            .flat_map(|c| c.html.split("</p>"))
            .map(|s| s.trim_start_matches("<p>").trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(
            items_text,
            vec![
                "Title: The Great Gatsby",
                "Author: F. Scott Fitzgerald",
                "* A Project Gutenberg of Australia eBook *",
                "prose after the pre",
            ],
            "each pre line its own paragraph, blanks dropped: {items_text:?}"
        );
    }

    /// A `<pre>` that an early boundary closes mid-block (a stray tag) keeps
    /// collecting lines: flush_text inside a pre emits the buffer and leaves
    /// the block open until the real `</pre>`.
    #[test]
    fn a_pre_block_survives_a_stray_boundary() {
        let html = "<pre>line one\nline two</pre><p>after</p>";
        let doc = parse(html.as_bytes()).unwrap();
        let text = doc.chapters.iter().map(|c| c.html.as_str()).collect::<String>();
        assert!(text.contains("<p>line one</p>"), "{text}");
        assert!(text.contains("<p>line two</p>"), "{text}");
        assert!(text.contains("<p>after</p>"), "{text}");
    }

    #[test]
    fn utf8_then_cp1252_decoding() {
        assert_eq!(decode("é—".as_bytes()), "é—");
        // 0x92 = ’ in cp1252; as UTF-8 a lone 0x92 is invalid.
        assert_eq!(decode(&[0x92]), "\u{2019}");
        assert_eq!(decode(&[0x93, 0x94]), "\u{201C}\u{201D}");
    }

    #[test]
    fn a_htmlz_carries_index_opf_cover_and_images() {
        // The fixture: a deflate-less zip writer would be a crate; the
        // Archive parser reads STORED entries, so the crate's own
        // `zip::stored_zip_files` builds one.
        let zip_bytes = super::super::zip::stored_zip_files(&[
            ("index.html", b"<html><head><title>T</title></head><body>\
                <p>before <img src=\"images/pic.png\" alt=\"a pic\"> after</p></body></html>"),
            ("metadata.opf", b"<?xml version=\"1.0\"?>\
                <package xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><metadata>\
                <dc:title>Htmlz Title</dc:title><dc:creator>Htmlz Author</dc:creator>\
                </metadata></package>"),
            ("images/pic.png", b"\x89PNG fake"),
            ("cover.jpg", b"\xFF\xD8\xFF fake jpeg"),
        ]);
        let doc = parse_htmlz(&zip_bytes).unwrap();
        assert_eq!(doc.title.as_deref(), Some("Htmlz Title"));
        assert_eq!(doc.author.as_deref(), Some("Htmlz Author"));
        let cover = doc.cover.as_ref().unwrap();
        assert_eq!(cover.name, "img/cover");
        assert_eq!(cover.mime, "image/jpeg");
        let image = doc.image("pic.png").unwrap();
        assert_eq!(image.mime, "image/png");
        assert!(doc.chapters[0].html.contains("<img src=\"img/pic.png\" alt=\"a pic\">"));
    }

    #[test]
    fn a_htmlz_without_any_html_is_refused() {
        let zip = super::super::zip::stored_zip_files(&[("notes.txt", b"x")]);
        let err = parse_htmlz(&zip).unwrap_err();
        assert!(err.to_string().contains("index.html"), "{err}");
    }

    #[test]
    fn a_htmlz_mirror_without_an_index_still_reads() {
        // Mirrors vary: some keep the page under its own name. The
        // reader takes the first HTML entry when there is no index.html.
        let zip = super::super::zip::stored_zip_files(&[("chapter01.html", b"<p>x</p>")]);
        let doc = parse_htmlz(&zip).unwrap();
        assert_eq!(doc.chapters.len(), 1);
    }
}
