//! FictionBook (`.fb2`) — the format the largest free Russian-language
//! ebook libraries ship in, and the friendliest non-EPUB format to read
//! because it is plain XML.
//!
//! What the parser takes:
//!
//! - `<description><title-info>` for title, author and language, so the
//!   shelf shows the book's own metadata rather than the file name.
//! - `<body>` sections. A `<title>` carrying `<level>1`–`2` opens a
//!   chapter; deeper titles stay inside as prose. `<subtitle>` joins its
//!   title after an em dash, the way the books print it.
//! - `<table>` becomes a real `<table>` the chapter XHTML carries — FB2
//!   wraps each cell's text in `<p>`, so the paragraph reader is what
//!   fills the cells.
//! - `<binary>` payloads (base64) referenced by `<image l:href="#id">`
//!   become the book's images; the one named in `<coverpage>` is the
//!   shelf cover. A payload nothing references, or one the reader cannot
//!   show (SVG), ships no bytes.
//! - Verse (`<v>`) reads as paragraphs, one per line.
//!
//! ## The de-facto heading fallback
//!
//! Plenty of FB2 files carry no `<title>` elements at all — the body is a
//! flat run of `<section>`s that begin with a bold line. The first
//! paragraph of a section is treated as a heading when it is *entirely*
//! inside `<strong>`/`<emphasis>`, short and not sentence-punctuated: the
//! convention in every such file, and the document's own words either way.
//! (The iOS reference tracked an emphasis *counter* that the closing tag
//! had already wound back to zero by the time the paragraph closed, so its
//! fallback could never fire; this one tracks "the emphasis never
//! closed".)
//!
//! Ported in part from the sibling speechnotes-ios `Fb2Parser.swift`, the
//! user's own reference implementation — minus its EPUB-conversion step:
//! the parse ends at the [`BookDoc`] and the file stays on disk verbatim.

use super::bookdoc::{xml_escape, BookChapter, BookDoc, BookDocError, BookImage, BookToc};
use super::xml::{Attr, Event, Parser as XmlParser};
use std::collections::HashMap;

/// Bound on one chapter's items — a section-less FB2 chunks the same way a
/// heading-less text file does.
const CHAPTER_ITEM_LIMIT: usize = 150;

/// Parse a `.fb2` file.
///
/// A parse that aborted with something collected keeps what it collected —
/// half a chapter beats none, and FB2 in the wild is often slightly
/// malformed. An empty parse is a refused import.
pub fn parse(data: &[u8]) -> Result<BookDoc, BookDocError> {
    let text = decode(data);
    let mut reader = Fb2Reader::new();
    let mut parser = XmlParser::new(&text);
    while let Ok(Some(event)) = parser.next_event() {
        reader.handle(event);
    }
    reader.finish()
}

/// The encoding the XML declaration names, decoded with the shared
/// codepage tables. FB2 files are UTF-8 or windows-1251 in practice; the
/// table covers the rest of the family for free.
pub fn decode(data: &[u8]) -> String {
    let head = String::from_utf8_lossy(&data[..data.len().min(1024)]);
    if let Some(name) = declared_encoding(&head) {
        if let Some(decoded) = super::codepages::decode_named(&name, data) {
            return decoded;
        }
    }
    String::from_utf8_lossy(data).into_owned()
}

/// The value of `encoding="…"` in an XML declaration, when the head of the
/// document carries one.
fn declared_encoding(head: &str) -> Option<String> {
    let at = head.find("encoding")?;
    let after = &head[at + "encoding".len()..];
    let equals = after.find('=')?;
    let after = after[equals + 1..].trim_start();
    let quote = after.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let end = after[1..].find(quote)?;
    Some(after[1..end + 1].to_string())
}

/// One thing the reader collected, in document order.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Item {
    Heading { text: String, level: usize },
    /// A paragraph's XHTML (already escaped).
    Paragraph(String),
    Table(Vec<Vec<Cell>>),
    /// An `<image>` reference by its FB2 id; resolved against the decoded
    /// binaries when the parse ends, dropped when nothing answers.
    Image { id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Cell {
    text: String,
    header: bool,
}

/// Where the next paragraph's words belong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    /// A `<p>` in the body: the words are a paragraph.
    Body,
    /// A `<p>` directly inside a `<title>`: the words ARE its title.
    Title,
    /// A `<subtitle>`: joins the title it follows.
    Subtitle,
}

/// The streaming reader. FB2 files run to tens of megabytes, so nothing is
/// held but the item stream and the decoded images.
struct Fb2Reader {
    items: Vec<Item>,

    // Title state.
    title_depth: usize,
    title_level: usize,
    pending: Pending,

    // The paragraph being built: its XHTML, its plain text, and whether an
    // emphasis element wrapped the whole thing.
    paragraph_html: String,
    paragraph_plain: String,
    in_paragraph: bool,
    emphasis_depth: usize,
    /// Text sat outside every emphasis element — the paragraph is not a
    /// title candidate.
    paragraph_left_emphasis: bool,
    /// The paragraph carried any emphasis at all. (Checked separately
    /// from the depth: by the time `</p>` closes, the `</strong>` has
    /// already wound the depth back to zero.)
    paragraph_had_emphasis: bool,
    /// The first paragraph of a section, while it is still a candidate.
    paragraph_is_candidate: bool,
    /// Text accumulated since the last inline boundary.
    text_buffer: String,
    /// Image references made by the paragraph being built.
    pending_image_ids: Vec<String>,

    // Section state: the emphasis fallback only looks at the FIRST
    // paragraph of a section.
    section_depth: usize,
    paragraphs_in_section: usize,

    // Table state. FB2 wraps each cell's text in `<p>`, so the paragraph
    // reader is what fills the cells; `in_cell` is where its output goes.
    table_depth: usize,
    in_cell: bool,
    table_rows: Vec<Vec<Cell>>,
    row_cells: Vec<Cell>,
    in_header_cell: bool,
    cell_buffer: String,

    // Binary (image payload) state.
    binary_id: Option<String>,
    binary_mime: String,
    binary_buffer: String,

    // Document context.
    in_body: bool,
    in_coverpage: bool,
    /// Inside `<title-info>`: the description block that carries the book's
    /// own metadata. FB2 names an author twice — `<title-info>` (the book)
    /// and `<document-info>` (the file's provenance; calibre writes the
    /// converter there) — and only the first is the author the shelf shows.
    in_title_info: bool,
    cover_image_id: Option<String>,
    /// The decoded binaries, keyed by their FB2 id.
    image_pool: HashMap<String, BookImage>,

    // Metadata. Each capture has its own buffer: sharing one produced
    // "en en" as the author on the reference's first fixture run, because
    // `<lang>en</lang>` was still in the buffer when `<first-name>` began.
    title_capture: Option<String>,
    language_capture: Option<String>,
    author_parts: Vec<String>,
    capturing_title: bool,
    capturing_language: bool,
    capturing_name: bool,
    capture_buffer: String,
}

impl Fb2Reader {
    fn new() -> Self {
        Self {
            items: Vec::new(),
            title_depth: 0,
            title_level: 1,
            pending: Pending::Body,
            paragraph_html: String::new(),
            paragraph_plain: String::new(),
            in_paragraph: false,
            emphasis_depth: 0,
            paragraph_left_emphasis: false,
            paragraph_had_emphasis: false,
            paragraph_is_candidate: false,
            text_buffer: String::new(),
            pending_image_ids: Vec::new(),
            section_depth: 0,
            paragraphs_in_section: 0,
            table_depth: 0,
            in_cell: false,
            table_rows: Vec::new(),
            row_cells: Vec::new(),
            in_header_cell: false,
            cell_buffer: String::new(),
            binary_id: None,
            binary_mime: "image/jpeg".to_string(),
            binary_buffer: String::new(),
            in_body: false,
            in_coverpage: false,
            in_title_info: false,
            cover_image_id: None,
            image_pool: HashMap::new(),
            title_capture: None,
            language_capture: None,
            author_parts: Vec::new(),
            capturing_title: false,
            capturing_language: false,
            capturing_name: false,
            capture_buffer: String::new(),
        }
    }

    fn handle(&mut self, event: Event) {
        match event {
            Event::Start { name, attrs } | Event::Empty { name, attrs } => {
                // The pull parser reports `<image/>` as an `Empty` and
                // `<binary>…</binary>` as `Start`/`End`; both carry the
                // element's own meaning here, so they land together.
                let name = local(&name);
                self.start(&name, &attrs);
            }
            Event::End { name } => self.end(&local(&name)),
            Event::Text(text) => self.text(&text),
            Event::Raw { .. } => {}
        }
    }

    fn start(&mut self, name: &str, attrs: &[Attr]) {
        match name {
            "body" => {
                self.in_body = true;
            }
            "coverpage" => {
                self.in_coverpage = true;
            }
            "title-info" => {
                self.in_title_info = true;
            }
            "binary" => {
                self.binary_id = Attr::get(attrs, "id").map(str::to_string);
                self.binary_mime = Attr::get(attrs, "content-type")
                    .unwrap_or("image/jpeg")
                    .to_string();
                self.binary_buffer.clear();
            }
            "image" => {
                let href = Attr::get(attrs, "l:href")
                    .or_else(|| Attr::get(attrs, "xlink:href"))
                    .or_else(|| Attr::get(attrs, "href"));
                if let Some(id) = href.and_then(|h| h.strip_prefix('#').map(str::to_string)) {
                    if self.in_coverpage && self.cover_image_id.is_none() {
                        self.cover_image_id = Some(id.clone());
                    }
                    // A coverpage image is the shelf cover, not body
                    // content — its reference must not leak into the
                    // first paragraph's image list.
                    if self.in_body && !self.in_coverpage {
                        self.pending_image_ids.push(id);
                    }
                }
            }
            "title" => {
                self.title_depth += 1;
                self.title_level = 1;
            }
            "p" | "v" | "text" => {
                self.pending = if self.title_depth > 0 {
                    Pending::Title
                } else {
                    Pending::Body
                };
                self.in_paragraph = true;
                self.paragraph_html.clear();
                self.paragraph_plain.clear();
                self.emphasis_depth = 0;
                self.paragraph_left_emphasis = false;
                self.paragraph_had_emphasis = false;
                if self.title_depth == 0 && self.section_depth > 0 && !self.in_cell {
                    self.paragraphs_in_section += 1;
                    self.paragraph_is_candidate = self.paragraphs_in_section == 1;
                } else {
                    self.paragraph_is_candidate = false;
                }
            }
            "subtitle" => {
                self.pending = if self.title_depth > 0 {
                    Pending::Subtitle
                } else {
                    Pending::Body
                };
                self.in_paragraph = true;
                self.paragraph_html.clear();
                self.paragraph_plain.clear();
                self.emphasis_depth = 0;
                self.paragraph_left_emphasis = false;
                self.paragraph_had_emphasis = false;
            }
            "strong" | "emphasis" => {
                self.flush_text();
                self.emphasis_depth += 1;
                self.paragraph_html
                    .push_str(if name == "strong" { "<strong>" } else { "<em>" });
            }
            "th" => {
                self.in_cell = true;
                self.in_header_cell = true;
                self.cell_buffer.clear();
            }
            "td" => {
                self.in_cell = true;
                self.in_header_cell = false;
                self.cell_buffer.clear();
            }
            "table" => {
                self.table_depth += 1;
                if self.table_depth == 1 {
                    self.table_rows.clear();
                }
            }
            "tr" => {
                self.row_cells.clear();
            }
            "section" => {
                self.section_depth += 1;
                self.paragraphs_in_section = 0;
            }
            "book-title" => {
                if self.in_title_info {
                    self.capturing_title = true;
                    self.capture_buffer.clear();
                }
            }
            "lang" => {
                if self.in_title_info {
                    self.capturing_language = true;
                    self.capture_buffer.clear();
                }
            }
            "first-name" | "middle-name" | "last-name" | "nick" => {
                // The title-info author only. A `<document-info>` author is
                // the file's provenance, not the book's writer.
                if self.in_title_info {
                    self.capturing_name = true;
                    self.capture_buffer.clear();
                }
            }
            _ => {}
        }
    }

    fn end(&mut self, name: &str) {
        match name {
            "binary" => self.finish_binary(),
            "p" | "v" | "text" | "subtitle" => self.close_paragraph(),
            "level" => {
                let value = tidy(&std::mem::take(&mut self.text_buffer));
                if let Ok(level) = value.parse::<usize>() {
                    if level > 0 {
                        self.title_level = level;
                    }
                }
            }
            "title" => {
                self.title_depth = self.title_depth.saturating_sub(1);
                self.pending = Pending::Body;
            }
            "strong" | "emphasis" => {
                self.flush_text();
                self.emphasis_depth = self.emphasis_depth.saturating_sub(1);
                self.paragraph_html
                    .push_str(if name == "strong" { "</strong>" } else { "</em>" });
            }
            "th" | "td" => {
                self.in_cell = false;
                let text = tidy(&std::mem::take(&mut self.cell_buffer));
                if self.table_depth > 0 {
                    self.row_cells.push(Cell {
                        text,
                        header: self.in_header_cell,
                    });
                }
            }
            "tr" => {
                if self.table_depth > 0 && !self.row_cells.is_empty() {
                    self.table_rows.push(std::mem::take(&mut self.row_cells));
                }
            }
            "table" => {
                self.table_depth = self.table_depth.saturating_sub(1);
                if self.table_depth == 0 && !self.table_rows.is_empty() {
                    self.items.push(Item::Table(std::mem::take(&mut self.table_rows)));
                }
            }
            "section" => {
                self.section_depth = self.section_depth.saturating_sub(1);
            }
            "coverpage" => {
                self.in_coverpage = false;
            }
            "title-info" => {
                self.in_title_info = false;
            }
            "body" => {
                self.in_body = false;
            }
            "book-title" => {
                let value = tidy(&std::mem::take(&mut self.capture_buffer));
                if !value.is_empty() && self.title_capture.is_none() {
                    self.title_capture = Some(value);
                }
                self.capturing_title = false;
            }
            "lang" => {
                let value = tidy(&std::mem::take(&mut self.capture_buffer));
                if !value.is_empty() && self.language_capture.is_none() {
                    self.language_capture = Some(value);
                }
                self.capturing_language = false;
            }
            "first-name" | "middle-name" | "last-name" | "nick" => {
                let value = tidy(&std::mem::take(&mut self.capture_buffer));
                if !value.is_empty() {
                    self.author_parts.push(value);
                }
                self.capturing_name = false;
            }
            _ => {}
        }
    }

    fn text(&mut self, text: &str) {
        if self.binary_id.is_some() {
            self.binary_buffer.push_str(text);
            return;
        }
        if self.capturing_title || self.capturing_language || self.capturing_name {
            self.capture_buffer.push_str(text);
            return;
        }
        if self.in_cell {
            // Text sitting directly in a `<td>` (FB2 allows it beside the
            // `<p>`-wrapped kind) belongs to the cell.
            self.cell_buffer.push_str(text);
            return;
        }
        if self.title_depth > 0 || self.in_paragraph {
            self.text_buffer.push_str(text);
        }
    }

    /// The buffered text joins the paragraph — escaped into its XHTML and
    /// kept plain for the heading heuristics.
    fn flush_text(&mut self) {
        let raw = std::mem::take(&mut self.text_buffer);
        if raw.is_empty() {
            return;
        }
        let tidied = tidy(&raw);
        if !tidied.is_empty() {
            self.paragraph_html.push_str(&xml_escape(&tidied));
            self.paragraph_plain.push_str(&tidied);
            if self.emphasis_depth == 0 {
                self.paragraph_left_emphasis = true;
            } else {
                self.paragraph_had_emphasis = true;
            }
        } else if self.paragraph_html.ends_with(|c: char| c.is_alphanumeric()) {
            // Whitespace between two inline spans: one space, so the words
            // do not run together when the markup renders.
            self.paragraph_html.push(' ');
            self.paragraph_plain.push(' ');
        }
    }

    fn close_paragraph(&mut self) {
        if self.title_depth > 0 {
            // Inside a `<title>` the words ARE the title — the title
            // element wraps them in `<p>` and `<subtitle>`, so they arrive
            // as ordinary paragraph events and nothing else distinguishes
            // them.
            self.flush_text();
            self.in_paragraph = false;
            let text = std::mem::take(&mut self.paragraph_plain);
            let text = text.trim().to_string();
            if text.is_empty() {
                return;
            }
            match self.pending {
                Pending::Subtitle => {
                    // The half after the em dash joins its title.
                    if let Some(Item::Heading { text: existing, .. }) = self.items.last_mut() {
                        existing.push_str(" — ");
                        existing.push_str(&text);
                    }
                }
                _ => {
                    self.items.push(Item::Heading {
                        text,
                        level: self.title_level,
                    });
                    self.pending = Pending::Subtitle;
                }
            }
            return;
        }

        self.flush_text();
        self.in_paragraph = false;
        let html = std::mem::take(&mut self.paragraph_html);
        let plain = std::mem::take(&mut self.paragraph_plain);
        let was_candidate = self.paragraph_is_candidate;
        self.paragraph_is_candidate = false;
        let empty = plain.trim().is_empty();

        // A cell paragraph's words belong to the cell, not to the item
        // stream — FB2 wraps every cell's text in `<p>`.
        if self.in_cell {
            if !empty {
                if !self.cell_buffer.is_empty() {
                    self.cell_buffer.push(' ');
                }
                self.cell_buffer.push_str(plain.trim());
            }
            self.pending_image_ids.clear();
            return;
        }

        if !empty {
            // The de-facto heading convention: a section's first paragraph
            // that never left its emphasis wrapper, is short and is not
            // sentence-punctuated, is its title.
            let fully_emphasized =
                self.paragraph_had_emphasis && !self.paragraph_left_emphasis;
            if was_candidate && fully_emphasized && plain.chars().count() <= 120
                && !ends_sentence(&plain)
            {
                self.items.push(Item::Heading {
                    text: plain.trim().to_string(),
                    level: 1,
                });
            } else {
                self.items.push(Item::Paragraph(html));
            }
        }
        // The images the paragraph referenced ride after it.
        let ids = std::mem::take(&mut self.pending_image_ids);
        for id in ids {
            self.items.push(Item::Image { id });
        }
    }

    fn finish_binary(&mut self) {
        let Some(id) = self.binary_id.take() else {
            self.binary_buffer.clear();
            return;
        };
        let buffer = std::mem::take(&mut self.binary_buffer);
        let Some(mime) = displayable_mime(&self.binary_mime) else {
            return;
        };
        let data = base64_decode(&buffer);
        if !data.is_empty() {
            self.image_pool.insert(
                id,
                BookImage {
                    // The book's own `img/…` name is decided when the
                    // reference resolves; this entry is the payload.
                    name: String::new(),
                    mime: mime.to_string(),
                    data,
                },
            );
        }
    }

    /// The parse's end: resolve the image references, promote the cover,
    /// chapterize. A parse that collected nothing is a refusal.
    fn finish(mut self) -> Result<BookDoc, BookDocError> {
        if self.items.is_empty() {
            return Err(BookDocError::Malformed(
                "no readable text in the FictionBook".to_string(),
            ));
        }
        // Only the binaries something referenced ship with the book: an
        // unreferenced `<binary>` is dead weight in memory. The names are
        // flat and ordered by first reference — `img/000000`, `img/000001`.
        let mut images: Vec<BookImage> = Vec::new();
        let mut used: HashMap<String, usize> = HashMap::new(); // fb2 id → index
        for item in &self.items {
            if let Item::Image { id } = item {
                if used.contains_key(id) {
                    continue;
                }
                if let Some(image) = self.image_pool.get(id) {
                    used.insert(id.clone(), images.len());
                    images.push(BookImage {
                        name: format!("img/{}", image_ref_name(images.len())),
                        mime: image.mime.clone(),
                        data: image.data.clone(),
                    });
                }
            }
        }
        // The cover: the image the `<coverpage>` named, under its own
        // name the shelf knows. A coverpage reference is not a body
        // reference, so it resolves against the payload pool directly.
        let cover = self
            .cover_image_id
            .as_deref()
            .and_then(|id| self.image_pool.get(id))
            .map(|image| BookImage {
                name: "img/cover".to_string(),
                mime: image.mime.clone(),
                data: image.data.clone(),
            });

        let title = self.title_capture.take();
        let author = if self.author_parts.is_empty() {
            None
        } else {
            Some(self.author_parts.join(" "))
        };
        let language = self.language_capture.take();

        let (chapters, toc) = chapterize(std::mem::take(&mut self.items), &used);
        Ok(BookDoc {
            title,
            author,
            language,
            cover,
            chapters,
            toc,
            images,
        })
    }
}

/// Splits the items into chapters at their level 1–2 headings; deeper
/// titles stay inside as prose. Image references resolve against `used`
/// (the fb2 id → image index map); a reference nothing answers is dropped.
fn chapterize(
    items: Vec<Item>,
    used: &HashMap<String, usize>,
) -> (Vec<BookChapter>, Vec<BookToc>) {
    let mut chapters: Vec<BookChapter> = Vec::new();
    let mut toc: Vec<BookToc> = Vec::new();
    let mut title: Option<String> = None;
    let mut html = String::new();
    let mut item_count = 0usize;

    fn flush(
        chapters: &mut Vec<BookChapter>,
        toc: &mut Vec<BookToc>,
        title: &mut Option<String>,
        html: &mut String,
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
                    item_count += 1;
                }
            }
            Item::Paragraph(fragment) => {
                html.push_str("<p>");
                html.push_str(&fragment);
                html.push_str("</p>");
                item_count += 1;
            }
            Item::Table(rows) => {
                html.push_str("<table>");
                for row in rows {
                    html.push_str("<tr>");
                    for cell in row {
                        let (open, close) = if cell.header {
                            ("<th>", "</th>")
                        } else {
                            ("<td>", "</td>")
                        };
                        html.push_str(open);
                        html.push_str(&xml_escape(&cell.text));
                        html.push_str(close);
                    }
                    html.push_str("</tr>");
                }
                html.push_str("</table>");
                item_count += 1;
            }
            Item::Image { id } => {
                if let Some(index) = used.get(&id) {
                    html.push_str(&format!(
                        "<img src=\"img/{}\" alt=\"\">",
                        image_ref_name(*index)
                    ));
                }
                // A reference nothing answers loses its tag — a broken
                // image icon helps nobody.
                item_count += 1;
            }
        }
        if item_count >= CHAPTER_ITEM_LIMIT {
            // The bound: the section title carries onto the continuation
            // chapter so the contents list never shows "Chapter N"
            // mid-section.
            let carried = title.take();
            flush(&mut chapters, &mut toc, &mut title, &mut html);
            title = carried;
            item_count = 0;
        }
    }
    flush(&mut chapters, &mut toc, &mut title, &mut html);

    (chapters, toc)
}

/// The `img/` name for image `index` — `000000`, `000001`, in order of
/// first reference. FB2 ids are arbitrary (`cover.jpg`, `{1F9A…}`); the
/// book's own names are flat and safe in a URL path.
fn image_ref_name(index: usize) -> String {
    format!("{index:06}")
}

/// Whitespace and NBSP tidy. FB2's typographic characters arrive as real
/// characters already; all this does is collapse the indentation
/// pretty-printed files carry inside `<p>`.
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

/// A sentence-ending punctuation mark — the mark that keeps a bold line
/// from reading as a title.
fn ends_sentence(text: &str) -> bool {
    text.ends_with(['.', ',', ';', '!', '?'])
}

/// A mime type the reader can show, or `None` — SVG and unknown payloads
/// are dropped rather than shipped as an image tag that renders nothing.
fn displayable_mime(mime: &str) -> Option<&'static str> {
    match mime.to_ascii_lowercase().as_str() {
        "image/jpeg" | "image/jpg" => Some("image/jpeg"),
        "image/png" => Some("image/png"),
        "image/gif" => Some("image/gif"),
        "image/webp" => Some("image/webp"),
        _ => None,
    }
}

/// The element name without its namespace prefix (`l:href` → `href`).
fn local(name: &str) -> String {
    name.rsplit(':').next().unwrap_or(name).to_string()
}

/// Base64, decoded streaming: six bits at a time, no padding required,
/// whitespace and stray characters skipped the way the reference's
/// `.ignoreUnknownCharacters` skips them. A truncated tail still decodes
/// the bytes it has.
fn base64_decode(input: &str) -> Vec<u8> {
    fn value(byte: u8) -> Option<u32> {
        match byte {
            b'A'..=b'Z' => Some((byte - b'A') as u32),
            b'a'..=b'z' => Some((byte - b'a' + 26) as u32),
            b'0'..=b'9' => Some((byte - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut accumulator = 0u32;
    let mut bits = 0u32;
    for byte in input.bytes() {
        let Some(value) = value(byte) else { continue };
        accumulator = (accumulator << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((accumulator >> bits) as u8);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A whole FB2 fixture: description with title/author/lang, two
    /// sections with titles, a table, a cover and one body image.
    const FIXTURE: &str = r##"<?xml version="1.0" encoding="utf-8"?>
<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0">
  <description>
    <title-info>
      <author><first-name>Lev</first-name><last-name>Tolstoy</last-name></author>
      <book-title>War and Peace</book-title>
      <lang>ru</lang>
    </title-info>
  </description>
  <body>
    <coverpage><image l:href="#cover.jpg"/></coverpage>
    <section>
      <title><p>Book One</p><subtitle>1805</subtitle></title>
      <p>Well, Prince, what do you think of it?</p>
      <p>The second paragraph of the first section.</p>
    </section>
    <section>
      <title><level>2</level><p>Chapter Two</p></title>
      <table><tr><th>Name</th><th>Qty</th></tr><tr><td><p>Apples</p></td><td><p>12</p></td></tr></table>
      <p>The table chapter's prose.</p>
    </section>
    <section>
      <p><strong>The Bold Line</strong></p>
      <p>Prose under the bold line.</p>
      <p>See the map <image l:href="#map.png"/>.</p>
    </section>
  </body>
  <binary id="cover.jpg" content-type="image/jpeg">aGVsbG8=</binary>
  <binary id="map.png" content-type="image/png">aGVsbG8gdG9v</binary>
  <binary id="svg-thing.svg" content-type="image/svg+xml">PHN2Zy8+</binary>
</FictionBook>"##;

    #[test]
    fn metadata_cover_and_chapters_come_out() {
        let doc = parse(FIXTURE.as_bytes()).unwrap();
        assert_eq!(doc.title.as_deref(), Some("War and Peace"));
        assert_eq!(doc.author.as_deref(), Some("Lev Tolstoy"));
        assert_eq!(doc.language.as_deref(), Some("ru"));
        // The cover is the binary the coverpage named.
        let cover = doc.cover.as_ref().unwrap();
        assert_eq!(cover.name, "img/cover");
        assert_eq!(cover.mime, "image/jpeg");
        assert_eq!(cover.data, b"hello");
        // Two titled sections, then the bold-line fallback.
        assert_eq!(doc.chapters.len(), 3);
        assert_eq!(doc.chapters[0].title.as_deref(), Some("Book One — 1805"));
        assert!(doc.chapters[0].html.contains("<p>Well, Prince"), "{:?}", doc.chapters);
        assert_eq!(doc.chapters[1].title.as_deref(), Some("Chapter Two"));
        // The TOC names the chapters in order.
        assert_eq!(doc.toc[0].label, "Book One — 1805");
        assert_eq!(doc.toc[1].label, "Chapter Two");
    }

    #[test]
    fn the_document_info_author_is_not_the_book_author() {
        // FB2 names an author twice: `<title-info>` (the book's metadata)
        // and `<document-info>` (the file's provenance — calibre writes the
        // converter there, or the same person a second time). Joining both
        // put "James Joyce James Joyce" on the shelf of every calibre-made
        // .fb2. Only title-info counts, and the name parts outside it
        // capture nothing at all.
        let with_provenance = r##"<?xml version="1.0" encoding="utf-8"?>
<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0">
  <description>
    <title-info>
      <author><first-name>James</first-name><last-name>Joyce</last-name></author>
      <book-title>Ulysses</book-title>
      <lang>en</lang>
    </title-info>
    <document-info>
      <author><first-name>calibre</first-name><nick>8.3.0</nick></author>
      <program-used>calibre 8.3.0</program-used>
      <id>b01e88d5</id>
    </document-info>
  </description>
  <body>
    <section><p>Stately, plump Buck Mulligan came from the stairhead.</p></section>
  </body>
</FictionBook>"##;
        let doc = parse(with_provenance.as_bytes()).unwrap();
        assert_eq!(doc.author.as_deref(), Some("James Joyce"), "{:?}", doc.author);
        assert_eq!(doc.title.as_deref(), Some("Ulysses"));
        assert_eq!(doc.language.as_deref(), Some("en"));
    }

    #[test]
    fn images_resolve_and_an_unshowable_payload_is_dropped() {
        let doc = parse(FIXTURE.as_bytes()).unwrap();
        // The body image ships in `images`; the SVG (which the reader
        // cannot show) does not. The cover rides in `doc.cover`, not
        // here — a coverpage reference is not a body reference.
        assert_eq!(doc.images.len(), 1, "{:?}", doc.images);
        assert!(doc.cover.is_some());
        let map = doc.images.iter().find(|i| i.mime == "image/png").unwrap();
        assert_eq!(map.data, b"hello too");
        assert_eq!(map.name, "img/000000", "{:?}", doc.images);
        // The chapter that referenced it carries the reference rewritten,
        // after the paragraph that made it.
        let last = &doc.chapters[2].html;
        assert!(last.contains("<img src=\"img/000000\""), "{last}");
        assert!(last.contains("See the map"), "{last}");
        assert!(last.find("See the map").unwrap() < last.find("<img").unwrap(), "{last}");
    }

    #[test]
    fn a_bold_first_paragraph_is_the_sections_title() {
        let doc = parse(FIXTURE.as_bytes()).unwrap();
        assert_eq!(doc.chapters[2].title.as_deref(), Some("The Bold Line"));
        assert!(doc.chapters[2].html.contains("Prose under the bold line."));
    }

    #[test]
    fn tables_take_their_p_wrapped_cells() {
        let doc = parse(FIXTURE.as_bytes()).unwrap();
        let html = &doc.chapters[1].html;
        assert!(html.contains("<table>"), "{html}");
        assert!(html.contains("<th>Name</th>"), "{html}");
        assert!(html.contains("<td>Apples</td>"), "{html}");
        // The cells' paragraphs must not leak into the item stream as
        // loose prose around the table.
        assert!(!html.contains("<p>Apples</p>"), "{html}");
    }

    #[test]
    fn the_cover_reference_does_not_leak_into_the_first_chapter() {
        let doc = parse(FIXTURE.as_bytes()).unwrap();
        let first = &doc.chapters[0].html;
        assert!(!first.contains("<img"), "{first}");
    }

    #[test]
    fn a_deeper_title_stays_inside_as_prose() {
        let fb2 = r##"<?xml version="1.0"?>
<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0">
  <body><section>
    <title><p>Part</p></title>
    <p>a.</p>
    <section><title><level>3</level><p>Sub</p></title><p>b.</p></section>
  </section></body>
</FictionBook>"##;
        let doc = parse(fb2.as_bytes()).unwrap();
        assert_eq!(doc.chapters.len(), 1);
        assert_eq!(doc.chapters[0].title.as_deref(), Some("Part"));
        assert!(doc.chapters[0].html.contains("<p>Sub</p>"), "{:?}", doc.chapters);
    }

    #[test]
    fn a_cyrillic_file_declaring_cp1251_decodes() {
        // "Привет" in windows-1251, as the declaration demands.
        let mut data = Vec::new();
        data.extend_from_slice(
            b"<?xml version=\"1.0\" encoding=\"windows-1251\"?><FictionBook><body><p>",
        );
        data.extend_from_slice(&[0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2]);
        data.extend_from_slice(b"</p></body></FictionBook>");
        let doc = parse(&data).unwrap();
        assert!(
            doc.chapters[0].html.contains("Привет"),
            "{}",
            doc.chapters[0].html
        );
    }

    #[test]
    fn an_empty_fictionbook_is_refused() {
        let err = parse(b"<?xml version=\"1.0\"?><FictionBook/>").unwrap_err();
        assert!(err.to_string().contains("no readable text"), "{err}");
    }

    #[test]
    fn base64_decode_knows_the_alphabet_and_ignores_the_noise() {
        assert_eq!(base64_decode("aGVsbG8="), b"hello");
        // Line-wrapped payload, the shape every real file carries.
        assert_eq!(base64_decode("aGVs\nbG8g\ndG9v"), b"hello too");
        // No padding at all — a truncated tail still decodes what it has.
        assert_eq!(base64_decode("aGVsbG8"), b"hello");
        assert_eq!(base64_decode("  aG Vs bG8= "), b"hello");
    }
}
