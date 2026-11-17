//! Office import: `.docx` in, one note out.
//!
//! A `.docx` is a ZIP whose `word/document.xml` is WordprocessingML. What a
//! note wants is the *text and its paragraph shape* — headings become the
//! markdown that the editor already renders, lists keep their markers, and a
//! table's rows survive as pipe rows because a note with a lost table is a
//! broken note. What it deliberately does not keep: fonts, colours, indents,
//! headers/footers, footnotes, comments, tracked changes, embedded objects —
//! the formats the note editor has no answer for.
//!
//! ## The traps the real files on this box set
//!
//! * **The style is an ATTRIBUTE, never text.** Word writes
//!   `<w:pStyle w:val="Heading1"/>` — a self-closed element whose value is
//!   `w:val`. Reading its text content finds nothing (it has none), so a
//!   parser that reads element text produces paragraphs with no heading
//!   levels at all. Word 2016/365, LibreOffice and Docs all write the
//!   attribute.
//! * **Runs are not paragraphs.** `<w:t>` inside `<w:r>` inside `<w:p>`; a
//!   paragraph is one line, however many runs, and a `<w:br/>` inside a run
//!   is a line break *within* that paragraph.
//! * **A property block is not prose.** `<w:pPr>`/`<w:rPr>` carry elements
//!   whose inner text is a *default* (`<w:sz w:val="24"/>` has none, but a
//!   `<w:tblStyle>`'s alternate content can), and tracking that with one
//!   "in properties" flag is what stops a paragraph's style text from
//!   landing in the note.
//! * **List numbering lives in the style, not the text.** `<w:numPr>` points
//!   into `numbering.xml`; without walking that tree the honest fallback is
//!   `•`→`-` for bullets and leaving ordered lists as prose, which is what
//!   most real exports carry anyway.
//! * **A cell is a paragraph stack.** `<w:tc>` holds `<w:p>`s; one cell =
//!   one markdown cell with the paragraphs joined by a space.
//! * **Images come through the rels.** `<w:drawing><a:blip r:embed="rId10"/>`
//!   names a relationship id; the media path is in `word/_rels/document.xml.
//!   rels`. Guessing from the media folder's order is wrong the moment a
//!   document references an image twice, or not at all.

use crate::xml::{Attr, Event, Parser as XmlParser};
use crate::zip::Archive;

/// The note-shaped text of one `.docx`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocxText {
    /// Markdown text: `#` headings, `-`/`1.` lists, `|` table rows, prose.
    pub text: String,
    /// Images in document order, with their zip entry paths (`word/media/…`),
    /// deduplicated.
    pub images: Vec<String>,
}

/// Parse one `.docx`'s bytes into note-shaped markdown.
pub fn parse(bytes: &[u8]) -> Result<DocxText, DocxError> {
    let archive = Archive::parse(bytes.to_vec())?;
    let xml_bytes = archive
        .read("word/document.xml")
        .map_err(|_| DocxError::NotADocx("no word/document.xml".into()))?;
    let xml = String::from_utf8_lossy(&xml_bytes);

    // The relationship id -> media entry map, walked once. A missing rels
    // part is normal (a text-only document) and means "no images".
    let rels = media_rels(&archive);
    let mut out = DocxText::default();

    // State is a small stack: are we inside a paragraph? a table cell? a
    // property block? Each closing element flushes its piece.
    let mut p = XmlParser::new(&xml);
    // The paragraph's accumulated text: runs, tabs, breaks.
    let mut para = String::new();
    // 0 = prose, 1..6 = heading level (from the paragraph's style).
    let mut para_kind: usize = 0;
    // The style value read from `<w:pStyle w:val="…"/>`, settled at pPr's
    // close — the element can self-close before the paragraph does.
    let mut ppr_style: Option<String> = None;
    // Inside `<w:tc>`: the cell's paragraphs joined. Inside a paragraph
    // that itself lives in a cell, text goes to the cell instead.
    let mut cell: Option<String> = None;
    let mut row: Vec<String> = Vec::new();
    let mut in_table = false;
    // List detection: `<w:numPr>` seen in this paragraph's properties.
    let mut in_list_item = false;
    // Nesting depth inside the property blocks `<w:pPr>`/`<w:rPr>`: text
    // under one is a default value, not prose.
    let mut prop_depth: usize = 0;

    // A malformed tail of XML loses the rest of the document, not the whole
    // import: the prose already walked is a better note than an error the
    // reader cannot act on. `Some(Ok(event))` ends the walk on the first
    // malformed event, keeping everything before it.
    while let Some(Ok(event)) = p.next_event().transpose() {
        match event {
            Event::Start { name, attrs } | Event::Empty { name, attrs } => {
                match local_name(&name).as_str() {
                    "p" => {
                        // A paragraph inside a cell: its text accumulates in
                        // the cell, so the top-level paragraph state resets
                        // only for paragraphs that are not in one.
                        if cell.is_none() {
                            para.clear();
                            para_kind = 0;
                            in_list_item = false;
                            ppr_style = None;
                        }
                    }
                    "tbl" => in_table = true,
                    "tr" => row.clear(),
                    "tc" => cell = Some(String::new()),
                    // A break or tab within the paragraph. Inside a cell a
                    // space is the honest join; markdown has no other form.
                    "br" | "tab" => {
                        let target = match cell.as_mut() {
                            Some(c) => c,
                            None => &mut para,
                        };
                        target.push(' ');
                    }
                    "numPr" => in_list_item = true,
                    // `<w:pStyle w:val="Heading1"/>` is self-closed in every
                    // real file, so `w:val` arrives on the Empty variant; the
                    // Start arm covers a writer that wraps it. Word's own
                    // attribute is namespaced (`w:val`); an unprefixed `val`
                    // comes from hand-written files and other writers.
                    "pStyle" => {
                        if let Some(value) = Attr::get(&attrs, "w:val")
                            .or_else(|| Attr::get(&attrs, "val"))
                            .or_else(|| Attr::get(&attrs, "w:Val"))
                        {
                            ppr_style = Some(value.to_string());
                        }
                    }
                    // Image relationship id on `<a:blip r:embed="…"/>`.
                    "blip" => {
                        if let Some(rid) = Attr::get(&attrs, "r:embed")
                            .or_else(|| Attr::get(&attrs, "embed"))
                        {
                            if let Some(path) = rels.get(rid) {
                                out.images.push(path.clone());
                            }
                        }
                    }
                    // Property blocks swallow their own text: see the Text
                    // arm below. `pPr` in particular contains `<w:rPr>`, so
                    // the depth is a counter, not a flag.
                    "pPr" | "rPr" | "sectPr" | "tcPr" | "trPr" | "tblPr" => {
                        prop_depth += 1;
                    }
                    _ => {}
                }
            }
            Event::Text(t) => {
                if prop_depth > 0 {
                    continue;
                }
                if let Some(c) = cell.as_mut() {
                    c.push_str(&t);
                } else {
                    para.push_str(&t);
                }
            }
            Event::End { name } => match local_name(&name).as_str() {
                "p" => {
                    if in_table && cell.is_some() {
                        // A paragraph inside a cell: its text belongs to the
                        // cell, joined across the cell's paragraphs by a
                        // space at the cell's close.
                        let own = std::mem::take(&mut para);
                        let own = own.trim();
                        if !own.is_empty() {
                            if let Some(c) = cell.as_mut() {
                                if !c.is_empty() {
                                    c.push(' ');
                                }
                                c.push_str(own);
                            }
                        }
                    } else {
                        flush_para(&mut para, para_kind, in_list_item, &mut out);
                    }
                    para_kind = 0;
                    in_list_item = false;
                    ppr_style = None;
                }
                "tc" => {
                    if let Some(c) = cell.take() {
                        row.push(c.trim().replace('\n', " "));
                    }
                }
                "tr" => {
                    //  a `<w:tc>` the file never closes still holds the
                    // row's last cell — take it before the row is written
                    // (the original own pipe-in-a-cell test pins this).
                    if let Some(c) = cell.take() {
                        row.push(c.trim().replace('\n', " "));
                    }
                    if !row.is_empty() {
                        out.text.push('|');
                        for c in &row {
                            out.text.push_str(&format!(" {} |", escape_cell(c)));
                        }
                        out.text.push('\n');
                    }
                    row.clear();
                }
                "tbl" => {
                    in_table = false;
                    // The table's header row needs a separator line after
                    // it; the editor's table parser takes the GFM form.
                    out.text = finish_table_separator(&out.text);
                }
                "pPr" => {
                    // The style settles the paragraph's kind. Done at pPr's
                    // close because a self-closed `<w:pStyle/>` arrives
                    // before the paragraph's text does.
                    if let Some(style) = ppr_style.take() {
                        if let Some(level) = heading_level(&style) {
                            para_kind = level;
                        }
                    }
                    // The pPr arm owns its own depth decrement — it cannot
                    // also match the generic arm below, so leaving this out
                    // pinned prop_depth at 1 forever and every Text event
                    // after the first paragraph's properties was swallowed.
                    prop_depth = prop_depth.saturating_sub(1);
                }
                "rPr" | "sectPr" | "tcPr" | "trPr" | "tblPr" => {
                    prop_depth = prop_depth.saturating_sub(1);
                }
                _ => {}
            },
            Event::Raw { .. } => {}
        }
    }
    // A trailing paragraph the XML forgot to close.
    if !in_table && !para.trim().is_empty() {
        flush_para(&mut para, para_kind, in_list_item, &mut out);
    }

    // Images in document order, deduplicated: a picture referenced twice is
    // one resource, and the note's markdown can reference it twice too.
    let mut seen = std::collections::HashSet::new();
    out.images.retain(|path| seen.insert(path.clone()));

    if out.text.is_empty() && !out.images.is_empty() {
        // An image-only document: the note is the images, referenced so the
        // editor renders them once they are copied into note-images/.
        for path in &out.images {
            let file = path.rsplit('/').next().unwrap_or(path);
            out.text.push_str(&format!("![](images/{file})\n"));
        }
    }
    Ok(out)
}

/// Emit one paragraph's line: a heading, a bullet, or prose as-is.
fn flush_para(para: &mut String, kind: usize, list: bool, out: &mut DocxText) {
    let trimmed = std::mem::take(para).trim().to_string();
    para.clear();
    if trimmed.is_empty() {
        return;
    }
    let line = if (1..=6).contains(&kind) {
        format!("{} {}", "#".repeat(kind), trimmed)
    } else if list {
        format!("- {trimmed}")
    } else {
        trimmed
    };
    out.text.push_str(&line);
    out.text.push('\n');
}

/// The element's local name, dropping any namespace prefix.
fn local_name(tag: &str) -> String {
    match tag.rfind(':') {
        Some(i) => tag[i + 1..].to_string(),
        None => tag.to_string(),
    }
}

/// The heading level a paragraph style name implies, or `None` for prose.
///
/// Word's built-in style ids are `Heading1`..`Heading9` (the OOXML spec caps
/// at 9), plus `Title` and `Subtitle`, which render as an h1 here because a
/// note has no cover-page concept and a note's first line is its title. The
/// comparison is case-insensitive and covers the German and French style ids
/// LibreOffice writes on a non-English install.
fn heading_level(style: &str) -> Option<usize> {
    let folded = style.trim().to_ascii_lowercase();
    let level = if folded == "title" || folded == "subtitle" {
        1
    } else {
        let digits = folded
            .strip_prefix("heading")
            .or_else(|| folded.strip_prefix("titre"))
            .or_else(|| folded.strip_prefix("überschrift"))
            .or_else(|| folded.strip_prefix("berschrift"))?;
        digits.parse::<usize>().ok()?
    };
    (1..=6).contains(&level).then_some(level)
}

/// Escape a pipe inside a table cell so the row keeps its column count.
fn escape_cell(cell: &str) -> String {
    cell.replace('|', "\\|")
}

/// After a `|`-row table, insert the GFM separator row under its first row.
///
/// `finish_table_separator` finds the first row of each run of rows and
/// writes `| --- | --- |` after it, matching the row's column count.
fn finish_table_separator(text: &str) -> String {
    //  the original looked at the *following* line, which writes a
    // separator after every row but a run's last and none for a one-row
    // table; this function's own contract — "finds the first row of each
    // run of rows" — looks at the *preceding* one instead. Same separators
    // for the multi-row cases the original handled, plus the one-row table.
    let mut out = String::with_capacity(text.len() + 64);
    let mut prev_was_row = false;
    for line in text.lines() {
        let is_row = line.starts_with('|') && line.ends_with('|');
        out.push_str(line);
        out.push('\n');
        if is_row && !prev_was_row {
            // The first row of a table run: count its cells.
            let columns = line.matches('|').count() - 1;
            out.push('|');
            for _ in 0..columns {
                out.push_str(" --- |");
            }
            out.push('\n');
        }
        prev_was_row = is_row;
    }
    out
}

/// Relationship id → `word/media/…` entry, from `word/_rels/document.xml.rels`.
///
/// A `.docx` image reference is indirect: the body names an `r:embed` id and
/// the rels part maps it to a target. Only relationships of type `.../
/// image` are kept, and the target is resolved against `word/` — a relative
/// target like `media/image1.png` is `word/media/image1.png`, and a
/// root-relative one (`/word/media/…`) drops its leading slash, the OPC
/// convention. Anything else is dropped, because an image that cannot be
/// named cannot be written into the note.
///
/// The rels part is walked by hand rather than through the pull parser: it is
/// a flat sequence of `<Relationship .../>` elements whose only content *is*
/// attributes, and parsing it twice (once for events, once for attributes)
/// buys nothing.
fn media_rels(archive: &Archive) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let Ok(bytes) = archive.read("word/_rels/document.xml.rels") else {
        return map;
    };
    let xml = String::from_utf8_lossy(&bytes);
    for element in rel_elements(&xml) {
        let id = element.get("Id").unwrap_or_default().to_string();
        let kind = element.get("Type").unwrap_or_default().to_string();
        let target = element.get("Target").unwrap_or_default().to_string();
        if id.is_empty() || target.is_empty() || !kind.ends_with("/image") {
            continue;
        }
        // The Type attribute's namespace varies by writer; the last path
        // segment is always "image".
        let is_image = kind.rsplit('/').next() == Some("image");
        if !is_image {
            continue;
        }
        let path = if let Some(rest) = target.strip_prefix('/') {
            rest.trim_start_matches('/').to_string()
        } else {
            format!("word/{target}")
        };
        map.entry(id).or_insert(path);
    }
    map
}

/// One `<Relationship/>` element's attributes.
struct RelElement(std::collections::HashMap<String, String>);

impl RelElement {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }
}

/// Split the rels part into its `<Relationship .../>` elements.
///
/// Attributes are read directly because this file carries nothing else. The
/// `<Relationships>` wrapper ALSO matches a `Relationship` prefix, so an
/// element is only kept when it carries an `Id` — the wrapper has none.
fn rel_elements(xml: &str) -> Vec<RelElement> {
    let mut out = Vec::new();
    for chunk in xml.split('<').skip(1) {
        let body = match chunk.split_once('>') {
            Some((body, _)) => body,
            None => continue,
        };
        if !body.starts_with("Relationship") || !body.contains("Id=") {
            continue;
        }
        let mut map = std::collections::HashMap::new();
        let mut rest = body["Relationship".len()..].trim_start();
        while !rest.is_empty() {
            let Some(eq) = rest.find('=') else { break };
            let name = rest[..eq].trim().to_string();
            rest = rest[eq + 1..].trim_start();
            let Some(quote) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else {
                break;
            };
            let after = &rest[1..];
            let Some(close) = after.find(quote) else { break };
            let raw = &after[..close];
            let value = crate::xml::decode_entities(raw)
                .unwrap_or_else(|| raw.to_string());
            if !name.is_empty() {
                map.insert(name, value);
            }
            rest = after[close + 1..].trim_start();
        }
        // A real `<Relationship/>` carries an Id; the `<Relationships>`
        // wrapper (which also matched the prefix) carries only an xmlns.
        if map.contains_key("Id") {
            out.push(RelElement(map));
        }
    }
    out
}

#[derive(Debug, thiserror::Error)]
pub enum DocxError {
    #[error("not a docx: {0}")]
    NotADocx(String),
    #[error("zip: {0}")]
    Zip(#[from] crate::zip::ZipError),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A zip built by hand, because the crate has no zip *writer* — only the
    /// reader the fixtures feed. Stored entries (no compression) keep the
    /// fixture bytes readable in a debugger.
    pub(crate) fn docx(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        let mut central: Vec<u8> = Vec::new();
        let mut offset: u32 = 0;
        for (name, data) in files {
            let name_bytes = name.as_bytes();
            let crc = crc32(data);
            out.extend_from_slice(&[0x50, 0x4b, 0x03, 0x04]);
            out.extend_from_slice(&[0x0a, 0x00]); // version needed
            out.extend_from_slice(&[0x00, 0x00]); // flags
            out.extend_from_slice(&[0x00, 0x00]); // method: stored
            out.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // time/date
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
            out.extend_from_slice(&[0x00, 0x00]); // extra len
            out.extend_from_slice(name_bytes);
            let local_len = out.len() as u32 - offset;
            out.extend_from_slice(data);

            central.extend_from_slice(&[0x50, 0x4b, 0x01, 0x02]);
            central.extend_from_slice(&[0x0a, 0x00, 0x14, 0x00]);
            central.extend_from_slice(&[0x00, 0x00]);
            central.extend_from_slice(&[0x00, 0x00]);
            central.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
            central.extend_from_slice(&[0x00, 0x00]); // extra len
            central.extend_from_slice(&[0x00, 0x00]); // comment len
            central.extend_from_slice(&[0x00, 0x00]); // disk number
            central.extend_from_slice(&[0x00, 0x00]); // internal attrs
            central.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // external attrs
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name_bytes);
            offset += local_len + data.len() as u32;
        }
        let central_start = out.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06]);
        out.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(central.len() as u32).to_le_bytes());
        out.extend_from_slice(&central_start.to_le_bytes());
        out.extend_from_slice(&[0x00, 0x00]);
        out
    }

    /// The CRC-32 (IEEE) the zip format checksums with.
    pub(crate) fn crc32(data: &[u8]) -> u32 {
        let mut table = [0u32; 256];
        for (i, slot) in table.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xedb88320 ^ (c >> 1) } else { c >> 1 };
            }
            *slot = c;
        }
        let mut crc = 0xffff_ffffu32;
        for &b in data {
            crc = table[((crc ^ b as u32) & 0xff) as usize] ^ (crc >> 8);
        }
        crc ^ 0xffff_ffff
    }

    /// The smallest byte string that is a real image: a 1x1 PNG.
    const PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8,
        0xcf, 0xc0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xdd, 0x8d, 0xb0, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    #[test]
    fn word_writes_its_style_as_an_attribute_and_that_is_what_makes_headings() {
        // The shape Word 2016/365, LibreOffice and Docs all emit: the style
        // id is w:val on a self-closed element. Reading its *text* yields an
        // empty paragraph, which is how the first version of this parser
        // lost every heading in every real file.
        let xml = br#"<?xml version="1.0"?>
<w:document xmlns:w="http://x"><w:body>
<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Chapter One</w:t></w:r></w:p>
<w:p><w:pPr><w:pStyle w:val="Heading2"/></w:pPr><w:r><w:t>A section</w:t></w:r></w:p>
<w:p><w:r><w:t>Plain prose.</w:t></w:r></w:p>
</w:body></w:document>"#;
        let out = parse(&docx(&[("word/document.xml", xml)])).unwrap();
        assert_eq!(out.text, "# Chapter One\n## A section\nPlain prose.\n");
    }

    #[test]
    fn the_style_name_is_read_when_the_properties_are_never_closed() {
        // Some writers emit `<w:pPr>` ... `</w:pPr>` late, others omit the
        // close entirely on a truncated file. The heading must survive the
        // first form and not corrupt the second.
        let xml = br#"<w:document xmlns:w="http://x"><w:body>
<w:p><w:pPr><w:pStyle w:val="Heading3"/><w:ind w:left="0"/></w:pPr>
<w:r><w:t>Deeply nested</w:t></w:r></w:p>
</w:body></w:document>"#;
        let out = parse(&docx(&[("word/document.xml", xml)])).unwrap();
        assert_eq!(out.text.trim(), "### Deeply nested");
    }

    #[test]
    fn title_and_subtitle_are_headings_too() {
        let xml = br#"<w:document xmlns:w="http://x"><w:body>
<w:p><w:pPr><w:pStyle w:val="Title"/><w:rPr><w:b/></w:rPr></w:pPr><w:r><w:t>The Book</w:t></w:r></w:p>
</w:body></w:document>"#;
        let out = parse(&docx(&[("word/document.xml", xml)])).unwrap();
        assert_eq!(out.text.trim(), "# The Book");
    }

    #[test]
    fn runs_join_into_one_paragraph_and_properties_are_not_prose() {
        let xml = br#"<w:document xmlns:w="http://x"><w:body>
<w:p><w:pPr><w:spacing w:after="159"/><w:rPr><w:sz w:val="24"/></w:rPr></w:pPr><w:r><w:rPr><w:b/></w:rPr><w:t>Bold</w:t></w:r><w:r><w:t> and plain</w:t></w:r></w:p>
</w:body></w:document>"#;
        let out = parse(&docx(&[("word/document.xml", xml)])).unwrap();
        assert_eq!(
            out.text.trim(),
            "Bold and plain",
            "the separator text inside a property block must never reach the note"
        );
    }

    #[test]
    fn a_table_survives_as_pipe_rows_with_a_separator() {
        // The real shape from a Word 365 file: tblPr, tblGrid, then rows of
        // cells, each cell holding one paragraph.
        let xml = br#"<w:document xmlns:w="http://x"><w:body><w:tbl>
<w:tblPr><w:tblStyle w:val="11"/></w:tblPr><w:tblGrid><w:gridCol w:w="1640"/><w:gridCol w:w="2068"/></w:tblGrid>
<w:tr><w:trPr></w:trPr><w:tc><w:tcPr><w:tcW w:w="1640"/></w:tcPr><w:p><w:r><w:t>Drug</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Dose</w:t></w:r></w:p></w:tc></w:tr>
<w:tr><w:tc><w:p><w:r><w:t>Amoxicillin</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>40 mg/kg</w:t></w:r></w:p></w:tc></w:tr>
</w:tbl></w:body></w:document>"#;
        let out = parse(&docx(&[("word/document.xml", xml)])).unwrap();
        assert!(out.text.contains("| Drug | Dose |"), "{:?}", out.text);
        assert!(
            out.text.contains("| --- | --- |"),
            "a GFM table needs its separator row: {:?}",
            out.text
        );
        assert!(out.text.contains("| Amoxicillin | 40 mg/kg |"));
    }

    #[test]
    fn a_pipe_in_a_cell_does_not_break_the_row() {
        let xml = br#"<w:document xmlns:w="http://x"><w:body><w:tbl>
<w:tr><w:tc><w:p><w:r><w:t>a | b</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>c</w:t></w:r></w:p></w:tr>
</w:tbl></w:body></w:document>"#;
        let out = parse(&docx(&[("word/document.xml", xml)])).unwrap();
        let row = out.text.lines().find(|l| l.contains("a")).unwrap();
        assert!(row.contains("a \\| b"), "{row}");
        // The separator row counts the columns, so the escape keeps the
        // table rectangular for the editor's parser.
        assert!(out.text.contains("| --- | --- |"), "{:?}", out.text);
    }

    #[test]
    fn a_bullet_list_keeps_its_marker() {
        let xml = br#"<w:document xmlns:w="http://x"><w:body>
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Ate the biscuit</w:t></w:r></w:p>
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Licked the spoon</w:t></w:r></w:p>
</w:body></w:document>"#;
        let out = parse(&docx(&[("word/document.xml", xml)])).unwrap();
        assert_eq!(out.text, "- Ate the biscuit\n- Licked the spoon\n");
    }

    #[test]
    fn an_image_resolves_through_the_relationships() {
        // The real indirection: the body names r:embed=rId10 and the rels
        // part maps it to media/image1.png.
        let xml = br#"<w:document xmlns:w="http://x" xmlns:r="http://r" xmlns:a="http://a"><w:body>
<w:p><w:r><w:drawing><a:blip r:embed="rId10"/></w:drawing></w:r></w:p>
</w:body></w:document>"#;
        let rels = br#"<?xml version="1.0"?>
<Relationships xmlns="http://x/rels">
<Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
<Relationship Id="rId10" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/>
</Relationships>"#;
        let out = parse(&docx(&[
            ("word/document.xml", xml),
            ("word/_rels/document.xml.rels", rels),
            ("word/media/image1.png", PNG),
        ]))
        .unwrap();
        assert_eq!(out.images, vec!["word/media/image1.png".to_string()]);
        // An image-only document is the images.
        assert!(out.text.contains("![](images/image1.png)"), "{:?}", out.text);
    }

    #[test]
    fn a_root_relative_media_target_resolves() {
        let xml = br#"<w:document xmlns:w="http://x" xmlns:a="http://a"><w:body>
<w:p><w:r><w:drawing><a:blip r:embed="rId1"/></w:drawing></w:r></w:p>
</w:body></w:document>"#;
        let rels = br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="/word/media/x.png"/></Relationships>"#;
        let out = parse(&docx(&[
            ("word/document.xml", xml),
            ("word/_rels/document.xml.rels", rels),
        ]))
        .unwrap();
        assert_eq!(out.images, vec!["word/media/x.png".to_string()]);
    }

    #[test]
    fn an_unresolvable_image_reference_is_dropped_not_fatal() {
        let xml = br#"<w:document xmlns:w="http://x" xmlns:a="http://a"><w:body>
<w:p><w:r><w:t>Text stays</w:t></w:r></w:p><w:p><w:r><w:drawing><a:blip r:embed="rId404"/></w:drawing></w:r></w:p>
</w:body></w:document>"#;
        let out = parse(&docx(&[("word/document.xml", xml)])).unwrap();
        assert_eq!(out.text.trim(), "Text stays");
        assert!(out.images.is_empty(), "a dangling r:embed is not an image");
    }

    #[test]
    fn an_empty_document_is_not_an_error() {
        let xml = b"<w:document xmlns:w=\"http://x\"><w:body/></w:document>";
        let out = parse(&docx(&[("word/document.xml", xml)])).unwrap();
        assert!(out.text.is_empty());
        assert!(out.images.is_empty());
    }

    #[test]
    fn a_document_without_word_xml_is_refused_by_name() {
        let out = parse(&docx(&[("somewhere/else.txt", b"hello")]));
        assert!(matches!(out, Err(DocxError::NotADocx(_))));
    }

    #[test]
    fn a_truncated_document_keeps_the_prose_it_already_read() {
        // A hand-edited file that ends inside a tag: the reader takes what it
        // has rather than failing the import (the EPUB chapter rule).
        let xml = b"<w:document xmlns:w=\"http://x\"><w:body><w:p><w:r><w:t>First line</w:t></w:r></w:p><w:p><w:r><w:t>Second";
        let out = parse(&docx(&[("word/document.xml", xml)])).unwrap();
        // The unterminated tag's text is character data to the end of the
        // file; the walk keeps it, because prose lost to a truncation is
        // worse than prose gained.
        assert_eq!(out.text.trim(), "First line\nSecond");
    }

    #[test]
    fn heading_levels_parse_from_every_writers_spelling() {
        assert_eq!(heading_level("Heading1"), Some(1));
        assert_eq!(heading_level("heading2"), Some(2));
        assert_eq!(heading_level("HEADING6"), Some(6));
        assert_eq!(heading_level("Heading7"), None, "past the note's 6");
        assert_eq!(heading_level("Heading9"), None);
        assert_eq!(heading_level("Title"), Some(1));
        assert_eq!(heading_level("Subtitle"), Some(1));
        assert_eq!(heading_level("Normal"), None);
        assert_eq!(heading_level(""), None);
        assert_eq!(heading_level("Heading"), None, "no digits, no level");
    }

    #[test]
    fn one_resource_per_image_even_when_referenced_twice() {
        let xml = br#"<w:document xmlns:w="http://x" xmlns:a="http://a"><w:body>
<w:p><w:r><w:drawing><a:blip r:embed="rId1"/></w:drawing></w:r></w:p>
<w:p><w:r><w:drawing><a:blip r:embed="rId1"/></w:drawing></w:r></w:p>
<w:p><w:r><w:drawing><a:blip r:embed="rId2"/></w:drawing></w:r></w:p>
</w:body></w:document>"#;
        let rels = br#"<Relationships xmlns="http://x/rels">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/one.png"/>
<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/two.png"/>
</Relationships>"#;
        let out = parse(&docx(&[
            ("word/document.xml", xml),
            ("word/_rels/document.xml.rels", rels),
        ]))
        .unwrap();
        assert_eq!(
            out.images,
            vec![
                "word/media/one.png".to_string(),
                "word/media/two.png".to_string()
            ]
        );
    }
}
