//! PDF text extraction: glyph decoding plus the papero layout algorithm,
//! ported from the user's designated standard (`papero-pdf-text-extractor`,
//! stated 2026-10-02).
//!
//! The glyphs come from two engines, one algorithm. Preferred:
//! [`crate::pdfium_source`] — PDFium (Chrome's engine) decodes every glyph
//! into a box, immune to the font edge cases a hand-written decoder owns.
//! Fallback: the content-stream interpreter below — `Tj`/`TJ`/`'`/`"` paint
//! text, `Tm`/`Td`/`TD`/`T*` position the pen, `Tf` picks the font,
//! `BT`/`ET` bound the text objects — which needs no library at all. Both
//! produce the same shape: every painted glyph becomes a [`Char`] with its
//! box, exactly what papero's layout pass consumes. What follows that (lines
//! by y-overlap, spans split at wide gaps, column gutters from a coverage
//! histogram, paragraphs by leading and indent, hyphen undo, headings by
//! size against the body size) is papero's algorithm with its thresholds,
//! engine-independent.
//!
//! Deliberately out of scope, and why: RULED tables and figure/formula
//! detection need the graphics state (rules, fills, images) neither glyph
//! source tracks; the reader wants prose and the speech pipeline wants
//! sentences. Rotated text keeps its stream order like papero's but is not
//! grouped. What IS carried over is everything that decides whether a page
//! reads as sentences or as rubble: the word-gap thresholds, the column
//! cut, the paragraph join rules, hyphenation undo, and ligature/symbol
//! normalisation. Two additions the real books forced: a page WITH a gutter
//! reads column by column (the note it carries in [`assemble_chars`]),
//! because a row-wise band walk interleaves two columns line by line; and a
//! column-aligned cell band — the unruled tables the books actually carry,
//! a cause grid beside body prose, a scale box with a marker column — reads
//! as rows of cells joined "; " (the note in [`table_rows_in_column`]),
//! detected from text geometry alone: no rules, no fills, and a two-column
//! band with nothing beside it is indistinguishable from prose sub-columns
//! and is left alone.

// Older copies of this file carried garbage runs — each a
// `\x00\x02\u{fffd}X` 5-byte run (one 4-NUL run in the tests) spliced in at
// ~4 KB block boundaries; they are removed here, each deletion verified
// against the older in-tree pdftext (e.g. "extracted text:", '\u{00AD}',
// "coverage histogram").
// The function `pdfium_is_the_engine` below is -authored (its original,
// crates__sn-doc__src__pdftext.rs____content__c5d411d4a17a, is cut mid-body);
// its contract is the doc comment the fragment preserved.

use std::collections::HashMap;

use crate::pdf::{Document, Obj};

/// One painted glyph: papero's `Char`. Coordinates are PDF points with the
/// origin at the TOP-LEFT (y grows down), so bboxes can be compared and
/// sorted as screen geometry.
#[derive(Debug, Clone)]
pub struct Char {
    pub c: char,
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
    /// Font's pixel size (the Tm scale), the number every threshold divides by.
    pub size: f64,
    /// A real space glyph stood before this char in the content stream. The
    /// stream's own statement of a word break — honoured even when the boxes
    /// overlap, because an OCR text layer paints them overlapping.
    pub space: bool,
    /// The paint run (one `Tj`/`TJ` string) this glyph belongs to. A line's
    /// reading order is its runs in x order, and a run's own order is the
    /// stream's — two runs whose boxes overlap must not interleave.
    pub run: u32,
}

impl Char {
    pub(crate) fn cy(&self) -> f64 {
        (self.y0 + self.y1) / 2.0
    }
}

/// One visual run of text on a line, split at wide gaps: papero's `Span`.
#[derive(Debug, Clone)]
pub struct Span {
    pub text: String,
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
    pub size: f64,
    /// Font weight/class hints for heading detection.
    pub bold: bool,
    pub mono: bool,
    /// A table row's pre-joined cells ("cell; cell"): reads as its own
    /// block, never merged into a neighbouring line or paragraph.
    pub row: bool,
}

impl Span {
    fn from(chars: &[&Char], bold: bool, mono: bool) -> Self {
        // Spaces the stream never painted appear as box gaps: a gap wider
        // than a word gap (papero's WORD_GAP_EM) becomes one. With real
        // /Widths advances the gap is the writer's intent. The stream's own
        // painted spaces ride as the `space` flag and are honoured whatever
        // the boxes say — an OCR text layer paints word gaps as overlaps.
        let mut text = String::with_capacity(chars.len());
        let mut prev: Option<&Char> = None;
        for c in chars {
            if let Some(p) = prev {
                let size = p.size.max(c.size).max(4.0);
                if c.space || c.x0 - p.x1 > size * WORD_GAP_EM {
                    text.push(' ');
                }
            }
            text.push(c.c);
            prev = Some(c);
        }
        // A painted space AND a gap produce two; collapse runs.
        while text.contains("  ") {
            text = text.replace("  ", " ");
        }
        let size = mode_size(chars);
        Span {
            text,
            x0: chars.iter().map(|c| c.x0).fold(f64::INFINITY, f64::min),
            y0: chars.iter().map(|c| c.y0).fold(f64::INFINITY, f64::min),
            x1: chars.iter().map(|c| c.x1).fold(f64::NEG_INFINITY, f64::max),
            y1: chars.iter().map(|c| c.y1).fold(f64::NEG_INFINITY, f64::max),
            size,
            bold,
            mono,
            row: false,
        }
    }
}

/// One laid-out block of prose: papero's `Block`, reduced to what a reader
/// and a speech plan need.
#[derive(Debug, Clone, PartialEq)]
pub struct PdfBlock {
    /// "heading" | "paragraph" | "list_item"
    pub kind: String,
    /// Heading level when kind == "heading": 1 = biggest.
    pub level: usize,
    pub text: String,
    pub page: usize,
}

/// One page's extracted text: blocks in reading order plus the joined prose.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PdfPageText {
    pub page: usize,
    pub blocks: Vec<PdfBlock>,
    /// The blocks' text joined with blank lines — the chapter pane's text.
    pub text: String,
}

/// papero's thresholds (its `layout.py` constants), in font-size ems and points.
const WORD_GAP_EM: f64 = 0.18;
const SPAN_GAP_EM: f64 = 0.9;
const JUSTIFIED_GAP_EM: f64 = 2.6;
const PARA_GAP_EM: f64 = 0.55;
const MIN_GUTTER: f64 = 6.0;
/// Cell edges align to a strip within a hair; a span starting this close
/// to a strip's edge belongs to the cell, not across it.
const CELL_EDGE_EPS: f64 = 1.0;
const GUTTER_STEP: f64 = 2.0;
/// Relative heading size that demotes a big line to "still body text".
const HEADING_MIN_RATIO: f64 = 1.15;
/// A heading's minimum glyph count — a one-word line is a stray label.
const HEADING_MIN_CHARS: usize = 3;

/// Extract text from every page of a document.
pub fn extract(doc: &Document) -> Vec<PdfPageText> {
    extract_pages(doc, 0, usize::MAX)
}

/// One page's painted glyphs, unparsed — the debugging eye into whichever
/// engine produced them. The layout pass consumes exactly this shape, so a
/// page that reads wrong can be asked where its boxes actually landed.
/// PDFium is preferred; [`extract_chars_interpreted`] forces the fallback.
pub fn extract_chars(doc: &Document, page: usize) -> Vec<Char> {
    #[cfg(not(test))]
    if let Some(chars) = crate::pdfium_source::PdfiumTextSource::open(doc)
        .map(|src| src.page_chars(page))
    {
        return chars;
    }
    let pages = doc.pages();
    match pages.get(page) {
        Some((_, page_dict)) if page_dict.contains_key("Contents") => {
            page_chars(doc, page_dict)
        }
        _ => Vec::new(),
    }
}

/// [`extract_chars`] with the pdfium source refused — the content-stream
/// interpreter only. The A/B eye: when a page reads differently across
/// engines, this is the other engine's answer.
pub fn extract_chars_interpreted(doc: &Document, page: usize) -> Vec<Char> {
    let pages = doc.pages();
    match pages.get(page) {
        Some((_, page_dict)) if page_dict.contains_key("Contents") => {
            page_chars(doc, page_dict)
        }
        _ => Vec::new(),
    }
}

/// Whether the pdfium source is the engine behind [`extract_pages`] and
/// [`extract_chars`] for this document: true in production when the library
/// bound, false in test builds (the interpreter is pinned there) and on any
/// box without the library. The quality gate uses it to decide whether a
/// second opinion from the interpreter can differ at all — when this is
/// false, the layer it read already IS the interpreter's answer.
//  body authored from the contract above (preserved verbatim by the
// original fragment crates__sn-doc__src__pdftext.rs____content__c5d411d4a17a,
// which is cut mid-body); the shape mirrors [`extract_chars`]'s pdfium probe.
pub fn pdfium_is_the_engine(doc: &Document) -> bool {
    #[cfg(not(test))]
    {
        crate::pdfium_source::PdfiumTextSource::open(doc).is_some()
    }
    #[cfg(test)]
    {
        let _ = doc;
        false
    }
}

/// Extract a page range (0-based, half-open) — the reader reads one chapter
/// at a time, and a 2107-page book must not cost 2107 pages of work to open.
///
/// In test builds the interpreter is pinned as the only engine: the unit
/// tests assert layout against hand-built fixtures whose bytes are too thin
/// for a real engine to agree on, and the pdfium source carries its own
/// tests. Production (the store, the reader) always prefers pdfium.
/// Assemble already-decoded glyphs into page text — the layout pass made
/// public for engines other than the two built in: the OCR lines feed the
/// same paragraphs, columns and headings the text layer gets. `page` is
/// 1-based in the result.
pub fn page_text_from_chars(_doc: &Document, page: usize, chars: Vec<Char>) -> String {
    assemble_chars(page, chars).text
}

/// Already-decoded glyph boxes (the OCR lines' pseudo-glyphs) through the
/// same paragraphs, columns and headings a text layer gets. `page` is 1-based
/// in the result.
//  authored for the original rapidocr's `pdftext::layout_text(chars)`
// call; no original of this wrapper surfaced.
pub fn layout_text(chars: Vec<Char>) -> PdfPageText {
    assemble_chars(1, chars)
}

pub fn extract_pages(doc: &Document, from: usize, to: usize) -> Vec<PdfPageText> {
    #[cfg(not(test))]
    if let Some(src) = crate::pdfium_source::PdfiumTextSource::open(doc) {
        let pages = doc.pages();
        let mut out = Vec::new();
        for (index, (_, page_dict)) in pages.iter().enumerate().take(to).skip(from) {
            let chars = if page_dict.contains_key("Contents") {
                src.page_chars(index)
            } else {
                Vec::new()
            };
            out.push(assemble(doc, index + 1, chars));
        }
        return out;
    }
    let pages = doc.pages();
    let mut out = Vec::new();
    for (index, (_, page_dict)) in pages.iter().enumerate().take(to).skip(from) {
        if page_dict.contains_key("Contents") {
            let chars = page_chars(doc, page_dict);
            out.push(assemble(doc, index + 1, chars));
        } else {
            out.push(PdfPageText {
                page: index + 1,
                blocks: Vec::new(),
                text: String::new(),
            });
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The content-stream interpreter
// ---------------------------------------------------------------------------

/// A font's code→char map, resolved from the three places a PDF carries it:
/// a standard encoding (bytes are Latin-1-ish), `/Encoding /Differences`
/// (glyph names), or `/ToUnicode` (a CMap). A subset font with raw indices
/// only answers through ToUnicode; without one its text is unreadable, and
/// the honest output is what the bytes say — nothing.
#[derive(Debug, Default, Clone)]
struct FontMap {
    /// code → text, covering every code the font paints. A code may stand
    /// for SEVERAL characters: a ligature glyph (`fi`, `ffi`) carries a
    /// multi-character /ToUnicode destination, and truncating it is what
    /// turned every "fifth" into "ffth" in the books that map ligatures.
    map: HashMap<u32, String>,
    /// code → advance width in 1/1000 em (the font's /Widths). Without it
    /// the pen drifts from the writer's intent and per-glyph-positioned
    /// text scrambles by x.
    widths: HashMap<u32, f64>,
}

impl FontMap {
    fn standard() -> Self {
        // The standard encodings agree with Latin-1 for printable ASCII and
        // the common punctuation; that is the 90% case for word-processor
        // exports (WinAnsi is Latin-1 with €, ™, typographic quotes).
        let mut map = HashMap::new();
        for b in 0u32..256 {
            let c = match b {
                0x18 | 0x19 => '‘',
                0x1A | 0x1B => '’',
                0x1C => '“',
                0x1D => '”',
                0x1E => '–',
                0x1F => '—',
                0x20..=0x7E => b as u8 as char,
                0x80 => '€',
                0x92 => '’',
                0x91 => '‘',
                0x93 => '“',
                0x94 => '”',
                0x95 => '•',
                0x96 => '–',
                0x97 => '—',
                0x99 => '™',
                0x85 => '…',
                0xA0..=0xFF => b as u8 as char,
                _ => continue,
            };
            map.insert(b, c.to_string());
        }
        FontMap {
            map,
            widths: HashMap::new(),
        }
    }

    /// Load one font's map from its dictionary.
    fn load(doc: &Document, dict: &HashMap<String, Obj>) -> FontMap {
        let mut map = FontMap::standard();
        // Simple fonts carry /Widths: FirstChar..LastChar per-1000 units.
        let first = doc.int_of(dict, "FirstChar").unwrap_or(0);
        if let Some(widths) = doc.array_of(dict, "Widths") {
            for (i, w) in widths.iter().enumerate() {
                if let Obj::Int(w) = w {
                    map.widths.insert((first + i as i64) as u32, *w as f64);
                } else if let Obj::Real(w) = w {
                    map.widths.insert((first + i as i64) as u32, *w);
                }
            }
        }
        // A Type0 font's widths live in its descendant's /W array, keyed by
        // CID: [c [w w w] cFirst cLast w cFirst2 cLast2 w2 ...].
        if let Some(descendants) = doc.array_of(dict, "DescendantFonts") {
            if let Some(Obj::Ref(id)) = descendants.first() {
                if let Some(d) = doc.get(*id).and_then(|o| o.as_dict(doc)) {
                    if let Some(w_array) = doc.array_of(d, "W") {
                        let mut i = 0;
                        while i < w_array.len() {
                            match (&w_array[i], w_array.get(i + 1)) {
                                (Obj::Int(c), Some(Obj::Int(last))) => {
                                    // c first..last single width
                                    if let Some(Obj::Int(w)) = w_array.get(i + 2) {
                                        for code in *c..=*last {
                                            map.widths.insert(code as u32, *w as f64);
                                        }
                                    }
                                    i += 3;
                                }
                                (Obj::Int(c), Some(Obj::Array(ws))) => {
                                    for (k, w) in ws.iter().enumerate() {
                                        let v = match w {
                                            Obj::Int(n) => *n as f64,
                                            Obj::Real(n) => *n,
                                            _ => 0.0,
                                        };
                                        map.widths.insert((*c + k as i64) as u32, v);
                                    }
                                    i += 2;
                                }
                                _ => i += 1,
                            }
                        }
                    }
                }
            }
        }
        // /Encoding may name a predefined encoding or carry /Differences.
        if let Some(enc) = doc.dict_of(dict, "Encoding") {
            if let Some(diff) = doc.array_of(enc, "Differences") {
                let mut code = 0u32;
                for item in diff {
                    match item {
                        Obj::Int(n) => code = *n as u32,
                        Obj::Name(name) => {
                            if let Some(c) = glyph_name(name) {
                                map.map.insert(code, c.to_string());
                            }
                            code += 1;
                        }
                        _ => {}
                    }
                }
            }
        }
        // /ToUnicode is a CMap stream: `bfchar` pairs and `bfrange` runs.
        if let Some(tu) = doc.lookup(dict, "ToUnicode") {
            if let Some(bytes) = doc.stream_data(tu) {
                let _ = parse_tounicode(&bytes, &mut map.map);
            }
        }
        map
    }

    fn get(&self, code: u32) -> Option<&str> {
        self.map.get(&code).map(String::as_str)
    }
}

/// A `/Differences` glyph name as a character. The standard names are a
/// fixed table; `uniXXXX` and `uXXXX` decode arithmetically.
fn glyph_name(name: &str) -> Option<char> {
    const NAMES: &[(&str, char)] = &[
        ("space", ' '), ("exclam", '!'), ("quotedbl", '"'), ("numbersign", '#'),
        ("dollar", '$'), ("percent", '%'), ("ampersand", '&'), ("quotesingle", '\''),
        ("parenleft", '('), ("parenright", ')'), ("asterisk", '*'), ("plus", '+'),
        ("comma", ','), ("hyphen", '-'), ("period", '.'), ("slash", '/'),
        ("zero", '0'), ("one", '1'), ("two", '2'), ("three", '3'), ("four", '4'),
        ("five", '5'), ("six", '6'), ("seven", '7'), ("eight", '8'), ("nine", '9'),
        ("colon", ':'), ("semicolon", ';'), ("less", '<'), ("equal", '='), ("greater", '>'),
        ("question", '?'), ("at", '@'),
        ("bracketleft", '['), ("backslash", '\\'), ("bracketright", ']'),
        ("asciicircum", '^'), ("underscore", '_'), ("grave", '`'),
        ("braceleft", '{'), ("bar", '|'), ("braceright", '}'), ("asciitilde", '~'),
        ("bullet", '•'), ("endash", '–'), ("emdash", '—'), ("quotedblleft", '“'),
        ("quotedblright", '”'), ("quoteleft", '‘'), ("quoteright", '’'),
        ("ellipses", '…'), ("ellipsis", '…'), ("trademark", '™'),
        ("copyright", '©'), ("registered", '®'), ("degree", '°'),
        ("plusminus", '±'), ("multiply", '×'), ("divide", '÷'),
        ("mu", 'μ'), ("alpha", 'α'), ("beta", 'β'), ("gamma", 'γ'), ("delta", 'δ'),
        ("epsilon", 'ε'), ("theta", 'θ'), ("lambda", 'λ'), ("pi", 'π'), ("sigma", 'σ'),
        ("phi", 'φ'), ("omega", 'ω'), ("Omega", 'Ω'), ("Delta", 'Δ'), ("Sigma", 'Σ'),
        ("ff", 'ﬀ'), ("fi", 'ﬁ'), ("fl", 'ﬂ'), ("ffi", 'ﬃ'), ("ffl", 'ﬄ'),
        ("A", 'A'), ("B", 'B'), ("C", 'C'), ("D", 'D'), ("E", 'E'), ("F", 'F'),
        ("G", 'G'), ("H", 'H'), ("I", 'I'), ("J", 'J'), ("K", 'K'), ("L", 'L'),
        ("M", 'M'), ("N", 'N'), ("O", 'O'), ("P", 'P'), ("Q", 'Q'), ("R", 'R'),
        ("S", 'S'), ("T", 'T'), ("U", 'U'), ("V", 'V'), ("W", 'W'), ("X", 'X'),
        ("Y", 'Y'), ("Z", 'Z'),
        ("a", 'a'), ("b", 'b'), ("c", 'c'), ("d", 'd'), ("e", 'e'), ("f", 'f'),
        ("g", 'g'), ("h", 'h'), ("i", 'i'), ("j", 'j'), ("k", 'k'), ("l", 'l'),
        ("m", 'm'), ("n", 'n'), ("o", 'o'), ("p", 'p'), ("q", 'q'), ("r", 'r'),
        ("s", 's'), ("t", 't'), ("u", 'u'), ("v", 'v'), ("w", 'w'), ("x", 'x'),
        ("y", 'y'), ("z", 'z'),
    ];
    if let Some((_, c)) = NAMES.iter().find(|(n, _)| *n == name) {
        return Some(*c);
    }
    if let Some(hex) = name.strip_prefix("uni") {
        if hex.len() == 4 {
            if let Ok(v) = u32::from_str_radix(hex, 16) {
                return char::from_u32(v);
            }
        }
    }
    if let Some(hex) = name.strip_prefix('u') {
        if hex.len() >= 4 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            if let Ok(v) = u32::from_str_radix(&hex[..hex.len().min(6)], 16) {
                return char::from_u32(v);
            }
        }
    }
    None
}

/// A /ToUnicode CMap: `beginbfchar <src> <dst> endbfchar` and `beginbfrange
/// <lo> <hi> <dst> endbfrange` — `<dst>` a single value, or an ARRAY of one
/// destination per code, which many CMap writers emit instead. Destinations
/// are UTF-16BE hex and may name several characters (a ligature's "fi" is
/// `00660069`); truncating them is what turned every "fifth" into "ffth".
fn parse_tounicode(bytes: &[u8], map: &mut HashMap<u32, String>) -> Option<()> {
    let text = String::from_utf8_lossy(bytes);
    let b = text.as_bytes();

    // The next `<hex>` at/after `i`; returns (contents, index past `>`).
    fn hex_at<'a>(text: &'a str, b: &[u8], mut i: usize) -> Option<(usize, &'a str, usize)> {
        while i < b.len() && b[i] != b'<' {
            i += 1;
        }
        if i >= b.len() {
            return None;
        }
        let start = i + 1;
        let close = b[start..].iter().position(|&c| c == b'>')? + start;
        Some((start, &text[start..close], close + 1))
    }

    let mut i = 0usize;
    while i < b.len() {
        if b[i..].starts_with(b"beginbfchar") {
            i += 11;
            while i < b.len() && !b[i..].starts_with(b"endbfchar") {
                if b[i] == b'<' {
                    let Some((_, src_hex, after_src)) = hex_at(text.as_ref(), b, i) else {
                        break; // no closing '>': the map ends here, honestly
                    };
                    let Ok(src) = u32::from_str_radix(src_hex.trim(), 16) else {
                        i = after_src; // a malformed source: skip the pair
                        continue;
                    };
                    let Some((_, dst, after_dst)) = hex_at(text.as_ref(), b, after_src) else {
                        break;
                    };
                    if let Some(s) = utf16_hex_to_string(dst) {
                        map.insert(src, s);
                    }
                    i = after_dst;
                } else {
                    i += 1;
                }
            }
            i += 10;
        } else if b[i..].starts_with(b"beginbfrange") {
            i += 12;
            while i < b.len() && !b[i..].starts_with(b"endbfrange") {
                if b[i] == b'<' {
                    let Some((_, lo_hex, after_lo)) = hex_at(text.as_ref(), b, i) else {
                        break;
                    };
                    let Ok(lo) = u32::from_str_radix(lo_hex.trim(), 16) else {
                        i = after_lo;
                        continue;
                    };
                    let Some((_, hi_hex, after_hi)) = hex_at(text.as_ref(), b, after_lo) else {
                        break;
                    };
                    let Ok(hi) = u32::from_str_radix(hi_hex.trim(), 16) else {
                        i = after_hi;
                        continue;
                    };
                    let hi = hi.min(lo + 65535);
                    // The third field is either `<dst>` (each code advances
                    // the destination) or `[<d1> <d2> …]` (one per code).
                    let after_ws = after_hi + b[after_hi..]
                        .iter()
                        .take_while(|c| c.is_ascii_whitespace())
                        .count();
                    if b.get(after_ws) == Some(&b'[') {
                        let mut code = lo;
                        let mut j = after_ws + 1;
                        while j < b.len() && b[j] != b']' && code <= hi {
                            if b[j] == b'<' {
                                let Some((_, dst, after_dst)) = hex_at(text.as_ref(), b, j)
                                else {
                                    break;
                                };
                                if let Some(s) = utf16_hex_to_string(dst) {
                                    map.insert(code, s);
                                }
                                code += 1;
                                j = after_dst;
                            } else {
                                j += 1;
                            }
                        }
                        // j sits on the `]` (or the end): the range is done.
                        i = j;
                    } else if let Some((_, dst, after_dst)) = hex_at(text.as_ref(), b, after_hi) {
                        if let Some(s) = utf16_hex_to_string(dst) {
                            if s.chars().count() == 1 {
                                let base = s.chars().next().unwrap() as u32;
                                for code in lo..=hi {
                                    if let Some(c) = char::from_u32(base + (code - lo)) {
                                        map.insert(code, c.to_string());
                                    }
                                }
                            } else {
                                // A multi-character destination in a range:
                                // "each code adds one" is meaningless for it;
                                // the writer meant it for `lo` alone.
                                map.insert(lo, s);
                            }
                        }
                        i = after_dst;
                    } else {
                        i = after_hi;
                    }
                } else {
                    i += 1;
                }
            }
            i += 10;
        } else {
            i += 1;
        }
    }
    Some(())
}

/// A CMap destination hex string as its characters: `"0041"` → "A",
/// `"00660069"` → "fi", `"D83DDE00"` → the astral char the surrogate pair
/// names. Groups of four hex digits are UTF-16BE units.
fn utf16_hex_to_string(hex: &str) -> Option<String> {
    let clean: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
    if clean.is_empty() || clean.len() % 4 != 0 {
        return None;
    }
    let mut units = Vec::with_capacity(clean.len() / 4);
    for k in (0..clean.len()).step_by(4) {
        units.push(u16::from_str_radix(&clean[k..k + 4], 16).ok()?);
    }
    Some(String::from_utf16_lossy(&units))
}

/// A text-painting state machine over one page's content streams.
struct Interpreter {
    chars: Vec<Char>,
    /// The page's fonts, resolved to code→char maps, keyed by resource name.
    fonts: HashMap<String, FontMap>,
    /// The current font's map (cloned at Tf; a page has one live font).
    current: FontMap,
    /// Two-byte codes (a Type0 font's /ToUnicode with codespace 2).
    twobyte: bool,
    /// Current font's flags.
    bold: bool,
    mono: bool,
    /// Text matrix: a, b, c, d, e, f (e,f = pen position).
    tm: [f64; 6],
    /// Line matrix (Tm resets both; Td shifts relative to line start).
    lm: [f64; 6],
    /// Font size from Tf.
    tf_size: f64,
    /// Leading from TL/TD.
    leading: f64,
    /// Horizontal scale (Tz, percent).
    hscale: f64,
    /// Character spacing (Tc, in text-space units).
    tchar: f64,
    /// Word spacing (Tw).
    tword: f64,
    /// A space code was seen and no glyph has consumed it yet: it survives
    /// to the next paint call, because writers end a TJ array on the space
    /// and start the next word in another.
    pending_space: bool,
    /// The paint run counter: one increment per painted string, so every
    /// glyph knows which `Tj`/`TJ` string it came from.
    run: u32,
    /// The CTM's translation+scale (q/Q/cm track it): text positions are
    /// text-space × CTM, and a content stream that moves a column with cm
    /// would otherwise paint both columns on top of each other.
    ctm: [f64; 6],
    /// The saved graphics states (q/Q).
    stack: Vec<[f64; 6]>,
}

impl Interpreter {
    fn new() -> Self {
        Self {
            chars: Vec::new(),
            fonts: HashMap::new(),
            current: FontMap::standard(),
            twobyte: false,
            bold: false,
            mono: false,
            tm: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            lm: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            tf_size: 12.0,
            leading: 12.0,
            hscale: 1.0,
            tchar: 0.0,
            tword: 0.0,
            pending_space: false,
            run: 0,
            ctm: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            stack: Vec::new(),
        }
    }

    /// Paint one string's glyphs at the pen. `x`/`y` are the pen; glyph boxes
    /// are size-sized boxes marching right by an estimated advance — good
    /// enough for line/column/paragraph geometry, which is all the layout
    /// pass asks of them.
    fn paint(&mut self, raw: &[u8], kind_is_hex: bool) {
        // Font size in PAGE points: the text matrix's scale through the CTM.
        let det = (self.ctm[0] * self.ctm[3] - self.ctm[1] * self.ctm[2]).abs().sqrt();
        let size = self.tf_size * (self.tm[0].abs().max(self.tm[3].abs())) * det.max(0.001);
        if size <= 0.0 {
            return;
        }
        // The string is CODES, not text: one or two bytes each, through the
        // font's map. A UTF-16BE BOM in the bytes means the writer put text
        // there directly — some generators do — and the map is skipped. One
        // code may stand for several characters (a ligature's ToUnicode):
        // the code's advance covers them all, sliced evenly across the
        // characters' boxes.
        let direct = raw.len() >= 2 && raw[0] == 0xFE && raw[1] == 0xFF;
        let items: Vec<(String, Option<u32>)> = if direct {
            decode_pdf_string(raw, kind_is_hex)
                .chars()
                .map(|c| (c.to_string(), None))
                .collect()
        } else {
            self.decode_codes(raw)
                .into_iter()
                .map(|(c, k)| (c, Some(k)))
                .collect()
        };
        if items.is_empty() {
            return;
        }
        self.run += 1;
        let run = self.run;
        let mut pen_x = self.tm[4];
        let pen_y = self.tm[5];
        // Tc/Tw live in TEXT space — they ride the text matrix's scale, the
        // same factor `size` carries. A writer that sets Tf 1 and puts the
        // real size in Tm (this file's own producer does exactly that) then
        // writes word gaps as character spacing: added unscaled, a 2.9pt
        // word gap reads as 0.36pt and "(Fig. 5.1) and" comes out "(Fig.
        // 5.1)and".
        let tsc = self.tm[0].abs().max(self.tm[3].abs());
        // Per-glyph character spacing in page points (Tc adds to EVERY
        // glyph's advance, spec 9.3.3) — one per code: a code that stands
        // for several characters (a ligature, a ToUnicode pair) is ONE
        // glyph to the writer.
        let tc = self.tchar * tsc * self.hscale;
        for (text, code) in items {
            let mut glyphs: Vec<char> = Vec::new();
            for ch in text.chars() {
                match ch {
                    ' ' | '\t' => {
                        self.pending_space = true;
                        pen_x += size * 0.5 * self.hscale
                            + tc
                            + self.tword * tsc * self.hscale;
                    }
                    '\n' | '\r' => {}
                    _ => {
                        // Ligatures and odd spaces expand HERE, into the
                        // glyph list the layout consumes: 'ﬁ' becomes "fi"
                        // as two characters sharing the glyph's box. Taking
                        // only the first char of the expansion — the shape
                        // this function had before — is what turned every
                        // "fifth" into "ffth" and "Specific" into "Specifc"
                        // in the books that set ﬁ/fi ligatures.
                        let normalized = normalize_char(ch);
                        if normalized.is_empty() {
                            continue;
                        }
                        glyphs.extend(normalized.chars());
                    }
                }
            }
            if glyphs.is_empty() {
                continue;
            }
            // The real advance when the font's /Widths names this glyph's
            // code; the estimate only covers what the font does not.
            let code_width = code
                .and_then(|code| self.current.widths.get(&code).copied())
                .map(|w| w / 1000.0 * size);
            let n = glyphs.len();
            let width = if let Some(w) = code_width {
                w
            } else {
                glyphs
                    .iter()
                    .map(|&c| char_advance(c, size))
                    .sum::<f64>()
            } * self.hscale
                + tc;
            let share = width / n as f64;
            // The box is the glyph's INK slot: the writer's character
            // spacing is a gap BETWEEN glyphs, not ink, so it comes off the
            // box's right edge (the pen still advances by the full share).
            // A producer that writes its word gaps as character spacing —
            // "(Fig. 5.1) and" was exactly that — otherwise paints the
            // space INTO the ')' and the layout reads "(Fig. 5.1)and".
            let ink = (share - tc / n as f64).max(share * 0.25);
            let mut consumed = false;
            for (k, &ch) in glyphs.iter().enumerate() {
                // A painted space rides as the flag on the glyph it preceded —
                // the stream's own word break, honoured even when this OCR-layer
                // file paints its word gaps as box overlaps. A synthetic space
                // glyph was tried here once: inserted at the pen, it landed ON
                // TOP of the next glyph and interleaved two runs' letters
                // ("th yeea r4" for "the 4th year"). One code's several
                // characters share the break: it lands on the first of them.
                let space = if consumed {
                    false
                } else {
                    consumed = true;
                    std::mem::take(&mut self.pending_space)
                };
                // PDF's y grows UP; the layout pass reads top-down. The mirror
                // at push time makes every box top-left-based (papero's
                // convention) without tracking the page height.
                let (x0, y_top) = ctm_apply(&self.ctm, pen_x + share * k as f64, pen_y + size);
                self.chars.push(Char {
                    c: ch,
                    x0,
                    y0: -y_top,
                    x1: x0 + ink,
                    y1: -y_top + size,
                    size,
                    space,
                    run,
                });
            }
            pen_x += width / det.max(0.001);
        }
        self.tm[4] = pen_x;
    }

    /// String bytes as font codes: one or two bytes each, through the
    /// font's map. A code the map does not name contributes nothing.
    fn decode_codes(&self, raw: &[u8]) -> Vec<(String, u32)> {
        let mut out = Vec::new();
        if self.twobyte {
            for pair in raw.chunks_exact(2) {
                let code = ((pair[0] as u32) << 8) | pair[1] as u32;
                if let Some(c) = self.current.get(code) {
                    out.push((c.to_string(), code));
                }
            }
        } else {
            for &b in raw {
                if let Some(c) = self.current.get(b as u32) {
                    out.push((c.to_string(), b as u32));
                }
            }
        }
        out
    }

    /// Run one content stream.
    fn run(&mut self, content: &[u8]) {
        // A token stream: operands accumulate until an operator lands.
        let mut operands: Vec<Operand> = Vec::new();
        let mut i = 0usize;
        let mut in_text = false;
        while i < content.len() {
            let b = content[i];
            match b {
                b'%' => {
                    while i < content.len() && content[i] != b'\n' {
                        i += 1;
                    }
                }
                b'(' => {
                    let (s, next) = read_literal(content, i);
                    operands.push(Operand::Str(s));
                    i = next;
                }
                b'<' if content.get(i + 1) == Some(&b'<') => {
                    // A dictionary: skip it — the interpreter reads none.
                    let mut depth = 0usize;
                    while i < content.len() {
                        if content[i] == b'<' && content.get(i + 1) == Some(&b'<') {
                            depth += 1;
                            i += 2;
                        } else if content[i] == b'>' && content.get(i + 1) == Some(&b'>') {
                            depth -= 1;
                            i += 2;
                            if depth == 0 {
                                break;
                            }
                        } else {
                            i += 1;
                        }
                    }
                }
                b'<' => {
                    let (s, next) = read_hex(content, i);
                    operands.push(Operand::Hex(s));
                    i = next;
                }
                b'[' => {
                    // An array (TJ's): collect its elements as one operand.
                    let mut depth = 0usize;
                    let start = i;
                    while i < content.len() {
                        match content[i] {
                            b'[' => depth += 1,
                            b']' => {
                                depth -= 1;
                                if depth == 0 {
                                    i += 1;
                                    break;
                                }
                            }
                            b'(' => {
                                let (_, next) = read_literal(content, i);
                                i = next;
                                continue;
                            }
                            b'<' if content.get(i + 1) != Some(&b'<') => {
                                let (_, next) = read_hex(content, i);
                                i = next;
                                continue;
                            }
                            b'%' => {
                                while i < content.len() && content[i] != b'\n' {
                                    i += 1;
                                }
                                continue;
                            }
                            _ => {}
                        }
                        i += 1;
                    }
                    operands.push(Operand::Array(content[start..i].to_vec()));
                }
                b'/' => {
                    let start = i + 1;
                    i += 1;
                    while i < content.len()
                        && !content[i].is_ascii_whitespace()
                        && !matches!(content[i], b'/' | b'<' | b'>' | b'[' | b']' | b'(' | b')')
                    {
                        i += 1;
                    }
                    let name = String::from_utf8_lossy(&content[start..i]).into_owned();
                    operands.push(Operand::Name(name));
                }
                b if b.is_ascii_digit() || b == b'-' || b == b'+' || b == b'.' => {
                    let start = i;
                    while i < content.len()
                        && (content[i].is_ascii_digit()
                            || matches!(content[i], b'-' | b'+' | b'.' | b'e' | b'E'))
                    {
                        i += 1;
                    }
                    let text = std::str::from_utf8(&content[start..i]).unwrap_or("0");
                    operands.push(Operand::Num(text.parse::<f64>().unwrap_or(0.0)));
                    // An operator keyword can follow a number with NO space
                    // ("BT" after "1"), and `b` may be its first letter — the
                    // number scan stopped at it. Handled below by the generic
                    // operator read; nothing to do here.
                }
                b if b.is_ascii_whitespace() => {
                    i += 1;
                }
                _ => {
                    // An operator: letters up to whitespace or a delimiter.
                    let start = i;
                    while i < content.len()
                        && !content[i].is_ascii_whitespace()
                        && !matches!(content[i], b'/' | b'<' | b'[' | b'(' | b')' | b']' | b'>' | b'{')
                    {
                        i += 1;
                    }
                    let op = std::str::from_utf8(&content[start..i]).unwrap_or("");
                    self.apply(op, &mut operands, &mut in_text);
                    // A stray delimiter lands here with an empty operator
                    // name (')' outside a string): without consuming it the
                    // loop spins on this byte forever. Every other branch
                    // advances; this one must too.
                    if i == start {
                        i += 1;
                    }
                }
            }
        }
    }

    /// Apply one operator to the accumulated operands.
    fn apply(&mut self, op: &str, operands: &mut Vec<Operand>, in_text: &mut bool) {
        match op {
            "BT" => {
                *in_text = true;
                self.tm = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
                self.lm = self.tm;
                // A new text object repositions the pen outright: a space
                // deferred from the previous object would be a word break
                // the writer never drew.
                self.pending_space = false;
            }
            "ET" => {
                *in_text = false;
            }
            "Tf" => {
                // size font-name → operands order: Tf takes (font size) with
                // the name BEFORE it: `/F1 12 Tf`. So Name is operands[0].
                let (name, size) = match operands.as_slice() {
                    [Operand::Name(n), Operand::Num(s)] => (n.clone(), *s),
                    _ => (String::new(), 0.0),
                };
                self.tf_size = size;
                if let Some(map) = self.fonts.get(&name) {
                    self.current = map.clone();
                    // A Type0 font's ToUnicode with a <0000>-style src is
                    // two-byte; a simple font's codes are one byte.
                    self.twobyte = map
                        .map
                        .keys()
                        .any(|k| *k > 0xFF);
                }
                self.bold = name.to_ascii_lowercase().contains("bold");
                self.mono = ["mono", "courier", "consol"]
                    .iter()
                    .any(|k| name.to_ascii_lowercase().contains(k));
            }
            "Tm" => {
                if let Some(m) = six(operands) {
                    self.tm = m;
                    self.lm = m;
                }
            }
            "Td" => {
                if let [Operand::Num(tx), Operand::Num(ty), ..] = operands.as_slice() {
                    // Td moves the line: tm AND lm both become T(tx,ty) × lm
                    // (PDF 9.4.3 — the line matrix follows the move, which is
                    // why a stream's per-glyph Td chain accumulates).
                    let [a, b, c, d, e, f] = self.lm;
                    let tm = [a, b, c, d, a * tx + c * ty + e, b * tx + d * ty + f];
                    self.tm = tm;
                    self.lm = tm;
                }
            }
            "TD" => {
                if let [Operand::Num(tx), Operand::Num(ty), ..] = operands.as_slice() {
                    self.leading = -*ty;
                    // TD sets a new line start: lm moves to the new position.
                    let [a, b, c, d, e, f] = self.lm;
                    let tm = [a, b, c, d, a * tx + c * ty + e, b * tx + d * ty + f];
                    self.tm = tm;
                    self.lm = tm;
                }
            }
            "T*" => {
                // Next line: lm itself moves down by the leading.
                let [a, b, c, d, e, f] = self.lm;
                self.tm = [a, b, c, d, e, f - self.leading];
                self.lm[5] = f - self.leading;
            }
            "TL" => {
                if let [Operand::Num(l), ..] = operands.as_slice() {
                    self.leading = *l;
                }
            }
            "Tj" | "'" => {
                if op == "'" {
                    // A quote moves to the next line: lm itself steps down.
                    let [a, b, c, d, e, f] = self.lm;
                    self.tm = [a, b, c, d, e, f - self.leading];
                    self.lm[5] = f - self.leading;
                }
                if let Some(Operand::Str(s)) = operands.last() {
                    self.paint(s, false);
                } else if let Some(Operand::Hex(s)) = operands.last() {
                    self.paint(s, true);
                }
            }
            "\"" => {
                // aw ac string: word & char spacing then a Td-led Tj.
                if let [Operand::Num(aw), Operand::Num(ac), Operand::Str(s), ..] =
                    operands.as_slice()
                {
                    self.tword = *aw;
                    self.tchar = *ac;
                    let [a, b, c, d, e, f] = self.lm;
                    self.tm = [a, b, c, d, e, f - self.leading];
                    self.lm[5] = self.tm[5];
                    self.paint(s, false);
                }
            }
            "TJ" => {
                if let Some(Operand::Array(arr)) = operands.last() {
                    self.paint_array(arr);
                }
            }
            "Tc" => {
                if let [Operand::Num(v), ..] = operands.as_slice() {
                    self.tchar = *v;
                }
            }
            "Tw" => {
                if let [Operand::Num(v), ..] = operands.as_slice() {
                    self.tword = *v;
                }
            }
            "Tz" => {
                if let [Operand::Num(v), ..] = operands.as_slice() {
                    self.hscale = *v / 100.0;
                }
            }
            "q" => {
                self.stack.push(self.ctm);
            }
            "Q" => {
                self.ctm = self.stack.pop().unwrap_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
            }
            "cm" => {
                if let Some(m) = six(operands) {
                    // cm post-multiplies: new CTM = cm × CTM.
                    self.ctm = mat_mul(&m, &self.ctm);
                }
            }
            _ => {}
        }
        operands.clear();
    }

    /// A TJ array: strings interleaved with numbers (kerning, in 1/1000 em).
    fn paint_array(&mut self, arr: &[u8]) {
        let mut i = 0usize;
        while i < arr.len() {
            match arr[i] {
                b'(' => {
                    let (s, next) = read_literal(arr, i);
                    self.paint(&s, false);
                    i = next;
                }
                b'<' if arr.get(i + 1) != Some(&b'<') => {
                    let (s, next) = read_hex(arr, i);
                    self.paint(&s, true);
                    i = next;
                }
                b'-' | b'+' | b'.' | b'0'..=b'9' => {
                    let start = i;
                    i += 1;
                    while i < arr.len() && (arr[i].is_ascii_digit() || matches!(arr[i], b'-' | b'+' | b'.')) {
                        i += 1;
                    }
                    // A displacement in 1/1000 em: positive pulls the pen
                    // left (kerning), negative pushes it right. A negative
                    // one this wide is a word gap the stream drew itself —
                    // the space glyph the writer omitted — and it belongs to
                    // the NEXT glyph, like a painted space.
                    let kern = std::str::from_utf8(&arr[start..i])
                        .ok()
                        .and_then(|s| s.parse::<f64>().ok())
                        .unwrap_or(0.0);
                    let size = self.tf_size * self.tm[0].abs().max(self.tm[3].abs());
                    self.tm[4] -= kern / 1000.0 * size * self.hscale;
                    if kern <= -150.0 {
                        self.pending_space = true;
                    }
                }
                b']' => break,
                _ => i += 1,
            }
        }
    }
}

/// 3x2 matrix multiply: a × b, in PDF's row-vector convention
/// (x' = x·a0 + y·a2 + a4 ...). `a` is applied AFTER b — a·b maps points
/// through b first.
fn mat_mul(a: &[f64; 6], b: &[f64; 6]) -> [f64; 6] {
    [
        a[0] * b[0] + a[1] * b[2],
        a[0] * b[1] + a[1] * b[3],
        a[2] * b[0] + a[3] * b[2],
        a[2] * b[1] + a[3] * b[3],
        a[4] * b[0] + a[5] * b[2] + b[4],
        a[4] * b[1] + a[5] * b[3] + b[5],
    ]
}

/// Apply the CTM to a text-matrix point: page position of the pen.
fn ctm_apply(m: &[f64; 6], x: f64, y: f64) -> (f64, f64) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

enum Operand {
    Num(f64),
    Str(Vec<u8>),
    Hex(Vec<u8>),
    Name(String),
    /// The raw bytes of a `[ ... ]` array, re-lexed by its consumer.
    Array(Vec<u8>),
}

fn six(operands: &[Operand]) -> Option<[f64; 6]> {
    if operands.len() >= 6 {
        let mut m = [0.0f64; 6];
        for (i, o) in operands.iter().take(6).enumerate() {
            match o {
                Operand::Num(n) => m[i] = *n,
                _ => return None,
            }
        }
        return Some(m);
    }
    None
}

/// A literal string's raw bytes with escapes decoded: `(…)` with `\n \r \t`,
/// octal, and balanced-paren nesting.
fn read_literal(data: &[u8], start: usize) -> (Vec<u8>, usize) {
    let mut i = start + 1;
    let mut out = Vec::new();
    let mut depth = 1usize;
    while i < data.len() {
        let b = data[i];
        i += 1;
        match b {
            b'(' => {
                depth += 1;
                out.push(b);
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
                out.push(b);
            }
            b'\\' => {
                if i >= data.len() {
                    break;
                }
                let e = data[i];
                i += 1;
                match e {
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'b' => out.push(0x08),
                    b'f' => out.push(0x0C),
                    b'0'..=b'7' => {
                        let mut v = (e - b'0') as u32;
                        let mut digits = 1;
                        while digits < 3 && i < data.len() && (b'0'..=b'7').contains(&data[i]) {
                            v = v * 8 + (data[i] - b'0') as u32;
                            i += 1;
                            digits += 1;
                        }
                        out.push((v & 0xFF) as u8);
                    }
                    b'\r' => {
                        // A line continuation: \r\n or \r vanish.
                        if data.get(i) == Some(&b'\n') {
                            i += 1;
                        }
                    }
                    b'\n' => {}
                    other => out.push(other),
                }
            }
            other => out.push(other),
        }
    }
    (out, i)
}

/// A hex string's bytes: `<48656C>`, whitespace ignored, odd digit padded 0.
fn read_hex(data: &[u8], start: usize) -> (Vec<u8>, usize) {
    let mut i = start + 1;
    let mut out = Vec::new();
    let mut hi: Option<u8> = None;
    while i < data.len() {
        let b = data[i];
        i += 1;
        if b == b'>' {
            break;
        }
        let v = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ => continue,
        };
        match hi.take() {
            None => hi = Some(v),
            Some(h) => out.push(h * 16 + v),
        }
    }
    if let Some(h) = hi {
        out.push(h * 16);
    }
    (out, i)
}

/// Decode a PDF string's bytes as text: UTF-16BE with a BOM, else the whole
/// byte sequence is the glyph source one Latin-1 byte per char (a simple
/// font's encoding is a single byte per glyph).
fn decode_pdf_string(raw: &[u8], _hex: bool) -> String {
    if raw.len() >= 2 && raw[0] == 0xFE && raw[1] == 0xFF {
        let units: Vec<u16> = raw[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    raw.iter().map(|&b| b as char).collect()
}

/// A rough per-glyph advance: the layout pass only needs relative widths.
/// CJK and other full-width scripts take a whole em — underestimating them
/// overlaps their boxes and merges distinct lines.
fn char_advance(c: char, size: f64) -> f64 {
    match c {
        'i' | 'j' | 'l' | 'I' | '\'' | '|' | '!' | '.' | ',' | ':' | ';' => size * 0.28,
        'm' | 'M' | 'W' | 'w' => size * 0.85,
        c if c.is_ascii_uppercase() => size * 0.66,
        c if c.is_ascii_lowercase() => size * 0.52,
        c if c.is_ascii_digit() => size * 0.55,
        c if (c as u32) >= 0x1100 && wide_script(c) => size, // CJK full-width
        _ => size * 0.55,
    }
}

/// The full-width ranges: CJK ideographs, kana, hangul, full-width forms.
fn wide_script(c: char) -> bool {
    let u = c as u32;
    (0x1100..=0x11FF).contains(&u)      // hangul jamo
        || (0x2E80..=0x9FFF).contains(&u) // CJK radicals..unified
        || (0xAC00..=0xD7AF).contains(&u) // hangul syllables
        || (0xF900..=0xFAFF).contains(&u) // CJK compat ideographs
        || (0xFF00..=0xFF60).contains(&u) // full-width forms
}

/// Ligatures and odd spaces: papero's `normalize_char`, the table this port
/// needs. A Symbol/Wingdings PUA glyph maps to its real symbol by code.
fn normalize_char(ch: char) -> String {
    match ch {
        '\u{FB00}' => "ff".into(),
        '\u{FB01}' => "fi".into(),
        '\u{FB02}' => "fl".into(),
        '\u{FB03}' => "ffi".into(),
        '\u{FB04}' => "ffl".into(),
        '\u{FB05}' | '\u{FB06}' => "st".into(),
        '\u{0092}' | '\u{2019}' => "’".into(),
        '\u{2018}' => "‘".into(),
        '\u{201C}' => "“".into(),
        '\u{201D}' => "”".into(),
        '\u{2013}' => "–".into(),
        '\u{2014}' => "—".into(),
        '\u{2022}' => "•".into(),
        '\u{00A0}' | '\u{2007}' | '\u{2009}' | '\u{200B}' => " ".into(),
        '\u{00AD}' | '\u{FEFF}' => String::new(),
        c if (c as u32) < 32 => String::new(),
        c => c.to_string(),
    }
}

fn mode_size(chars: &[&Char]) -> f64 {
    // The modal size to the half point — papero's `_mode_size`.
    let mut sizes: Vec<i64> = chars.iter().map(|c| (c.size * 2.0).round() as i64).collect();
    sizes.sort_unstable();
    if sizes.is_empty() {
        return 10.0;
    }
    let first = sizes[0];
    if sizes.last() == Some(&first) {
        return first as f64 / 2.0;
    }
    let mut best = first;
    let mut best_n = 0;
    let mut run_start = 0;
    for i in 1..=sizes.len() {
        if i == sizes.len() || sizes[i] != sizes[run_start] {
            let n = i - run_start;
            if n > best_n {
                best_n = n;
                best = sizes[run_start];
            }
            run_start = i;
        }
    }
    best as f64 / 2.0
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if values.is_empty() {
        return 0.0;
    }
    let mid = values.len() / 2;
    if values.len() % 2 == 0 {
        (values[mid - 1] + values[mid]) / 2.0
    } else {
        values[mid]
    }
}

// ---------------------------------------------------------------------------
// Page assembly
// ---------------------------------------------------------------------------

fn page_chars(doc: &Document, page: &HashMap<String, Obj>) -> Vec<Char> {
    let mut interp = Interpreter::new();
    // The page's /Resources /Font names each font's map. A page that
    // inherits resources from its parent (a tree, not a list) is rare; the
    // walk below follows one level of it.
    let resources = doc
        .dict_of(page, "Resources")
        .map(|r| r.to_owned())
        .unwrap_or_default();
    if let Some(fonts) = doc.dict_of(&resources, "Font") {
        for (name, value) in fonts {
            if let Some(dict) = value.as_dict(doc) {
                interp.fonts.insert(name.clone(), FontMap::load(doc, dict));
            }
        }
    }
    match page.get("Contents") {
        Some(Obj::Ref(id)) => {
            if let Some(obj) = doc.get(*id) {
                if let Some(data) = doc.stream_data(obj) {
                    interp.run(&data);
                }
            }
        }
        Some(Obj::Stream { .. }) => {
            if let Some(data) = doc.stream_data(page.get("Contents").expect("matched")) {
                interp.run(&data);
            }
        }
        Some(Obj::Array(items)) => {
            for item in items {
                if let Some(data) = doc.stream_data(item) {
                    interp.run(&data);
                }
            }
        }
        _ => {}
    }
    interp.chars
}

/// The papero layout pass over one page's glyphs: lines → spans → bands →
/// columns → paragraphs → blocks.
fn assemble(_doc: &Document, page: usize, chars: Vec<Char>) -> PdfPageText {
    //  the original assemble took the document directly; the split below
    // exists so the original rapidocr's `pdftext::layout_text(chars)` has an
    // entry point — assemble never reads the document (its parameter was
    // already unused in the original).
    assemble_chars(page, chars)
}

/// The layout pass proper, document-free: see [`assemble`].
fn assemble_chars(page: usize, mut chars: Vec<Char>) -> PdfPageText {
    chars.retain(|c| c.x1 > c.x0);
    if chars.is_empty() {
        return PdfPageText {
            page,
            blocks: Vec::new(),
            text: String::new(),
        };
    }

    // A run painted twice over the same place (a slide title's outline
    // under its fill, a shadow copy) reads once. The drop must happen
    // BEFORE lines are grouped: the layout's x-jump split below would
    // otherwise cut a wide shadow into its own raw line, where no
    // line-level collapse could ever see it beside the original.
    chars = drop_shadow_runs(chars);

    // --- body size: the modal glyph size of the page (papero's `body`).
    let mut size_tally: HashMap<i64, usize> = HashMap::new();
    for c in &chars {
        *size_tally.entry((c.size * 2.0).round() as i64).or_default() += 1;
    }
    let body = size_tally
        .iter()
        .max_by_key(|(_, n)| **n)
        .map(|(s, _)| *s as f64 / 2.0)
        .unwrap_or(10.0)
        .max(4.0);

    // --- lines: glyphs whose boxes overlap vertically are one visual line.
    // A jump clear of that — a glyph starting well right of the pen, or
    // well left of it — is not a word gap and not a kern: it is where
    // another column or another cell begins. The real books chain a table
    // row, its continuation line and the body prose beside it into one
    // y-chain (their boxes overlap through the 4–5 pt stagger), and the
    // x-interleaved runs then fuse cell fragments into the row's words
    // ("Shortslight of hill breath"). A visual line of prose never jumps:
    // its glyphs advance, word gaps ride under the same ceiling the span
    // splitter already trusts. So a jump this wide breaks the line, and
    // each column's glyphs group alone.
    chars.sort_by(|a, b| a.cy().partial_cmp(&b.cy()).unwrap_or(std::cmp::Ordering::Equal).then(a.x0.partial_cmp(&b.x0).unwrap_or(std::cmp::Ordering::Equal)));
    let mut raw_lines: Vec<Vec<Char>> = Vec::new();
    let mut cur: Vec<Char> = Vec::new();
    for ch in chars {
        if let Some(last) = cur.last() {
            let overlap = last.y1.min(ch.y1) - last.y0.max(ch.y0);
            let small = last.size.min(ch.size);
            let same_line = overlap >= small * 0.35
                && ch.x0 - last.x1 <= small * JUSTIFIED_GAP_EM
                && last.x1 - ch.x0 <= small * JUSTIFIED_GAP_EM;
            if !same_line {
                raw_lines.push(std::mem::take(&mut cur));
            }
        }
        cur.push(ch);
    }
    if !cur.is_empty() {
        raw_lines.push(cur);
    }

    // --- run order: a line's reading order is its paint runs in x order,
    // and a run keeps its own order. Sorting glyphs alone by x interleaves
    // two runs whose boxes overlap — an OCR text layer overlaps its word
    // gaps, and letters from different runs came out as "th yeea r4" for
    // "the 4th year". Duplicate runs (a slide title's shadow copy — the
    // same words painted twice over the same place) collapse to one.
    let raw_lines: Vec<Vec<Char>> = raw_lines.into_iter().map(order_line_runs).collect();

    // --- spans: each line splits at gaps much wider than its word gaps.
    let mut spans: Vec<Span> = Vec::new();
    for line in &raw_lines {
        let size = mode_size(&line.iter().collect::<Vec<_>>()).max(4.0);
        // The line's typical word gap, from the gaps that are word-sized.
        let mut gaps: Vec<f64> = Vec::new();
        for pair in line.windows(2) {
            let g = pair[1].x0 - pair[0].x1;
            if g > 0.0 && g < size * WORD_GAP_EM * 3.0 {
                gaps.push(g);
            }
        }
        let typical = if gaps.len() >= 3 { median(gaps) } else { 0.0 };
        let mut piece: Vec<Char> = Vec::new();
        for i in 0..line.len() {
            if i > 0 {
                let prev = &line[i - 1];
                let ch = &line[i];
                let gap = ch.x0 - prev.x1;
                let wide = gap > size * SPAN_GAP_EM
                    && (gap > typical * 2.2 || gap > size * JUSTIFIED_GAP_EM)
                    && !(ch.space && gap < size * JUSTIFIED_GAP_EM);
                if wide {
                    let slice: Vec<&Char> = piece.iter().collect();
                    spans.push(Span::from(
                        &slice,
                        false,
                        false,
                    ));
                    piece.clear();
                }
            }
            piece.push(line[i].clone());
        }
        let slice: Vec<&Char> = piece.iter().collect();
        spans.push(Span::from(&slice, false, false));
    }
    spans.retain(|s| !s.text.trim().is_empty());

    if spans.is_empty() {
        return PdfPageText {
            page,
            blocks: Vec::new(),
            text: String::new(),
        };
    }

    // --- columns: gutters from the coverage histogram (papero's columns.py).
    let boxes: Vec<(f64, f64, f64, f64)> = spans
        .iter()
        .map(|s| (s.x0, s.y0, s.x1, s.y1))
        .collect();
    let gutters = find_gutters(&boxes);
    let cols = column_edges(&boxes, &gutters);

    // --- table bands: within each page column, a y-range where a clear
    // vertical strip separates column-aligned cell blocks reads as rows of
    // cells ("cell; cell") — the note in [`table_rows_in_column`]. The
    // rows replace their cells in the reading order as pre-joined spans.
    let mut row_spans: Vec<Span> = Vec::new();
    let mut consumed = vec![false; spans.len()];
    let mut by_col: Vec<Vec<usize>> = vec![Vec::new(); cols.len()];
    for (i, s) in spans.iter().enumerate() {
        by_col[col_of_box(s.x0, s.x1, &cols)].push(i);
    }
    for group in by_col.iter().filter(|g| g.len() >= 4) {
        let col_spans: Vec<&Span> = group.iter().map(|&i| &spans[i]).collect();
        let (rows, used) = table_rows_in_column(&col_spans);
        row_spans.extend(rows);
        for u in used {
            consumed[group[u]] = true;
        }
    }
    let spans: Vec<Span> = spans
        .into_iter()
        .zip(consumed)
        .filter_map(|(s, used)| if used { None } else { Some(s) })
        .chain(row_spans)
        .collect();

    // Reading order. A page WITH a gutter reads column by column — the
    // band-per-row walk below interleaves two columns line by line
    // ("left1 right1 left2 right2…"), which is rubble for the reader and
    // nonsense for the speech plan. A span that crosses the gutter (a
    // running head, a banner) cuts the page into horizontal bands and is
    // read where it sits; within a band, each column reads top-down,
    // columns left to right. A page WITHOUT a gutter keeps the band walk,
    // which for one column is plain top-down.
    let mut ordered: Vec<&Span> = Vec::new();
    if !gutters.is_empty() {
        let crosses = |s: &Span| {
            gutters.iter()
                .any(|g| s.x0 < g.0 - 3.0 && s.x1 > g.1 + 3.0)
        };
        let mut sorted: Vec<&Span> = spans.iter().collect();
        sorted.sort_by(|a, b| {
            a.y0.partial_cmp(&b.y0).unwrap_or(std::cmp::Ordering::Equal)
                .then(a.x0.partial_cmp(&b.x0).unwrap_or(std::cmp::Ordering::Equal))
        });
        let mut band: Vec<&Span> = Vec::new();
        for s in sorted {
            if crosses(s) {
                // Flush the band, then the crossing span reads here.
                band.sort_by(|a, b| {
                    col_of_box(a.x0, a.x1, &cols).cmp(&col_of_box(b.x0, b.x1, &cols))
                        .then(a.y0.partial_cmp(&b.y0).unwrap_or(std::cmp::Ordering::Equal))
                        .then(a.x0.partial_cmp(&b.x0).unwrap_or(std::cmp::Ordering::Equal))
                });
                ordered.append(&mut band);
                ordered.push(s);
            } else {
                band.push(s);
            }
        }
        band.sort_by(|a, b| {
            col_of_box(a.x0, a.x1, &cols).cmp(&col_of_box(b.x0, b.x1, &cols))
                .then(a.y0.partial_cmp(&b.y0).unwrap_or(std::cmp::Ordering::Equal))
                .then(a.x0.partial_cmp(&b.x0).unwrap_or(std::cmp::Ordering::Equal))
        });
        ordered.append(&mut band);
    } else {
        let mut rest: Vec<&Span> = spans.iter().collect();
        while !rest.is_empty() {
            // The band: everything whose vertical range overlaps the top-most
            // span's core (papero pads a span's core by 22% each side).
            let top = *rest
                .iter()
                .min_by(|a, b| {
                    a.y0.partial_cmp(&b.y0).unwrap_or(std::cmp::Ordering::Equal)
                })
                .expect("non-empty");
            let core0 = top.y0 + (top.y1 - top.y0) * 0.22;
            let core1 = top.y1 - (top.y1 - top.y0) * 0.22;
            let mut band: Vec<&Span> = Vec::new();
            let mut next_rest: Vec<&Span> = Vec::new();
            for s in rest {
                let c0 = s.y0 + (s.y1 - s.y0) * 0.22;
                let c1 = s.y1 - (s.y1 - s.y0) * 0.22;
                if c0 < core1 && core0 < c1 {
                    band.push(s);
                } else {
                    next_rest.push(s);
                }
            }
            // Band members into column order.
            band.sort_by(|a, b| {
                let ca = col_of(a, &cols).unwrap_or(usize::MAX);
                let cb = col_of(b, &cols).unwrap_or(usize::MAX);
                ca.cmp(&cb).then(a.y0.partial_cmp(&b.y0).unwrap_or(std::cmp::Ordering::Equal))
            });
            ordered.extend(band);
            rest = next_rest;
        }
    }

    // --- lines of the reading order, then paragraphs by gap + indent.
    struct Ln {
        spans: Vec<Span>,
        x0: f64,
        y0: f64,
        x1: f64,
        y1: f64,
        size: f64,
        bold: bool,
        /// A table row: reads as its own block, never merged into a
        /// neighbouring line or paragraph.
        row: bool,
    }
    let mut lines: Vec<Ln> = Vec::new();
    for s in &ordered {
        // A span joins the open line only when it sits in the SAME column
        // and overlaps it vertically: two columns of a page share every y,
        // and merging across them splices the texts together. A row's
        // pre-joined cells never join, and nothing joins onto a row.
        let sc = col_of_box(s.x0, s.x1, &cols);
        let same_col = lines
            .last()
            .map(|last: &Ln| col_of_box(last.x0, last.x1, &cols) == sc)
            .unwrap_or(false);
        match lines.last_mut() {
            Some(last) if same_col && !s.row && !last.row && s.y0 < last.y1 - last.size * 0.35 => {
                last.spans.push((*s).clone());
                last.x0 = last.x0.min(s.x0);
                last.x1 = last.x1.max(s.x1);
                last.y0 = last.y0.min(s.y0);
                last.y1 = last.y1.max(s.y1);
                last.bold = last.bold && s.bold;
            }
            _ => lines.push(Ln {
                spans: vec![(*s).clone()],
                x0: s.x0,
                y0: s.y0,
                x1: s.x1,
                y1: s.y1,
                size: s.size,
                bold: s.bold,
                row: s.row,
            }),
        }
    }

    // The column each line sits in, for indent decisions.
    let mut blocks: Vec<PdfBlock> = Vec::new();
    let mut group: Vec<&Ln> = Vec::new();
    let flush = |group: &mut Vec<&Ln>, blocks: &mut Vec<PdfBlock>| {
        if group.is_empty() {
            return;
        }
        let mut text = String::new();
        let mut prev: Option<&Ln> = None;
        for (i, ln) in group.iter().enumerate() {
            let t = spans_text(&ln.spans);
            if text.is_empty() {
                text = t;
            } else if text.ends_with('-')
                && text.len() > 1
                && text.as_bytes()[text.len() - 2].is_ascii_alphabetic()
                && t.chars().next().is_some_and(|c| c.is_lowercase())
            {
                // A hyphen at the join with a lower-case continuation is
                // hyphenation, not a dash: undo it.
                text.pop();
                text.push_str(&t);
            } else {
                // A line that ends well short of its column's right edge
                // while the next line would have fit — the author's break.
                let right_edge = group
                    .iter()
                    .filter(|other| {
                        col_of_box(other.x0, other.x1, &cols)
                            == col_of_box(ln.x0, ln.x1, &cols)
                    })
                    .map(|other| other.x1)
                    .fold(f64::NEG_INFINITY, f64::max);
                let room = right_edge - prev.map(|p| p.x1).unwrap_or(0.0);
                let hard = i > 0 && room > ln.size * 3.0;
                if hard {
                    text.push('\n');
                } else {
                    text.push(' ');
                }
                text.push_str(&t);
            }
            prev = Some(ln);
        }
        let size = median(group.iter().map(|l| l.size).collect()).max(4.0);
        let all_bold = group.iter().all(|l| l.bold);
        // A list item opens with a bullet or an enumerator.
        let is_list = is_list_start(&text);
        // A heading: bigger than the body, short, no sentence punctuation.
        let is_heading = !is_list
            && (size >= body * HEADING_MIN_RATIO || (all_bold && size >= body * 1.02))
            && text.len() >= HEADING_MIN_CHARS
            && text.len() <= 120
            && !text.ends_with('.')
            && group.len() <= 3;
        let level = if is_heading {
            if size >= body * 1.7 {
                1
            } else if size >= body * 1.35 {
                2
            } else {
                3
            }
        } else {
            0
        };
        let text_stripped = text.trim().to_string();
        if !text_stripped.is_empty() {
            blocks.push(PdfBlock {
                kind: if is_list {
                    "list_item".into()
                } else if is_heading {
                    "heading".into()
                } else {
                    "paragraph".into()
                },
                level,
                text: text_stripped,
                page,
            });
        }
        group.clear();
    };

    for (i, ln) in lines.iter().enumerate() {
        if let Some(prev) = group.last() {
            let gap = ln.y0 - prev.y1;
            let overlap = prev.x1.min(ln.x1) - prev.x0.max(ln.x0);
            let same_style = (ln.size - prev.size).abs() <= prev.size.max(4.0) * 0.15;
            let continues = !ln.row
                && !prev.row
                && gap <= prev.size.max(4.0) * PARA_GAP_EM + prev.size.max(4.0) * 0.15
                && overlap > 0.0
                && same_style
                // A first-line indent opens a new paragraph.
                && !(ln.x0 > prev.x0 + prev.size.max(4.0) * 0.9)
                && !is_list_start(&spans_text(&ln.spans));
            if !continues {
                flush(&mut group, &mut blocks);
            }
        }
        let _ = i;
        group.push(ln);
    }
    flush(&mut group, &mut blocks);

    let text = blocks
        .iter()
        .map(|b| b.text.clone())
        .collect::<Vec<_>>()
        .join("\n\n");
    PdfPageText { page, blocks, text }
}

/// Runs painted twice over the same place at the same size read once: a
/// slide title's shadow copy, the outline under a fill — one "Lecture", not
/// "LectureLecture". Same predicate as the line-level collapse in
/// [`order_line_runs`], but across the whole stream: the x-jump split below
/// would cut a wide shadow into its own raw line, where no line-level pass
/// could ever see it beside the original. Chars keep their stream order.
fn drop_shadow_runs(chars: Vec<Char>) -> Vec<Char> {
    // One paint run's glyphs are contiguous in the stream.
    let mut runs: Vec<Vec<usize>> = Vec::new();
    for (i, ch) in chars.iter().enumerate() {
        match runs.iter_mut().find(|r| chars[r[0]].run == ch.run) {
            Some(r) => r.push(i),
            None => runs.push(vec![i]),
        }
    }
    let run_text = |r: &[usize]| -> String { r.iter().map(|&i| chars[i].c).collect() };
    let run_box = |r: &[usize]| -> (f64, f64, f64, f64) {
        (
            r.iter().map(|&i| chars[i].x0).fold(f64::INFINITY, f64::min),
            r.iter().map(|&i| chars[i].y0).fold(f64::INFINITY, f64::min),
            r.iter().map(|&i| chars[i].x1).fold(f64::NEG_INFINITY, f64::max),
            r.iter().map(|&i| chars[i].y1).fold(f64::NEG_INFINITY, f64::max),
        )
    };
    let run_size = |r: &[usize]| -> f64 { r.iter().map(|&i| chars[i].size).fold(0.0, f64::max) };
    let mut dropped = vec![false; chars.len()];
    let mut kept: Vec<(String, f64, f64, f64, f64, f64)> = Vec::new(); // text, box, size
    for run in &runs {
        let text = run_text(run);
        let (x0, y0, x1, y1) = run_box(run);
        let size = run_size(run);
        let shadow = kept.iter().any(|(kt, kx0, ky0, kx1, ky1, ks)| {
            let iw = x1.min(*kx1) - x0.max(*kx0);
            let ih = y1.min(*ky1) - y0.max(*ky0);
            let inter = iw.max(0.0) * ih.max(0.0);
            let area = ((x1 - x0) * (y1 - y0)).min((*kx1 - *kx0) * (*ky1 - *ky0)).max(1.0);
            text == *kt && inter / area > 0.7 && (size - *ks).abs() <= ks.max(4.0) * 0.15
        });
        if shadow {
            for &i in run {
                dropped[i] = true;
            }
        } else {
            kept.push((text, x0, y0, x1, y1, size));
        }
    }
    if dropped.iter().any(|d| *d) {
        chars
            .into_iter()
            .enumerate()
            .filter(|(i, _)| !dropped[*i])
            .map(|(_, c)| c)
            .collect()
    } else {
        chars
    }
}

/// One line's chars reordered: the line's runs in x order, each run's chars
/// in the stream's own order, duplicates of an earlier run dropped.
fn order_line_runs(line: Vec<Char>) -> Vec<Char> {
    // Group by run id, preserving arrival order (the line vec is x-sorted,
    // so a run's chars arrive in x order already).
    let mut runs: Vec<Vec<Char>> = Vec::new();
    for ch in line {
        match runs.iter_mut().find(|r| r[0].run == ch.run) {
            Some(r) => r.push(ch),
            None => runs.push(vec![ch]),
        }
    }
    // A duplicate run: the same text as an earlier run, painted over the
    // same place at the same size. A shadow copy of a slide title, or the
    // outline under a fill — one "Lecture", not "LectureLecture".
    let mut kept: Vec<Vec<Char>> = Vec::new();
    'next_run: for run in runs {
        let text: String = run.iter().map(|c| c.c).collect();
        let (x0, y0, x1, y1) = run_box(&run);
        for k in &kept {
            let ktext: String = k.iter().map(|c| c.c).collect();
            let (kx0, ky0, kx1, ky1) = run_box(k);
            let iw = x1.min(kx1) - x0.max(kx0);
            let ih = y1.min(ky1) - y0.max(ky0);
            let inter = (iw.max(0.0)) * (ih.max(0.0));
            let area = ((x1 - x0) * (y1 - y0)).min((kx1 - kx0) * (ky1 - ky0)).max(1.0);
            let size_a = run.iter().map(|c| c.size).fold(0.0, f64::max);
            let size_b = k.iter().map(|c| c.size).fold(0.0, f64::max);
            if text == ktext
                && inter / area > 0.7
                && (size_a - size_b).abs() <= size_b.max(4.0) * 0.15
            {
                continue 'next_run;
            }
        }
        kept.push(run);
    }
    kept.sort_by(|a, b| {
        a.iter().map(|c| c.x0).fold(f64::INFINITY, f64::min)
            .partial_cmp(&b.iter().map(|c| c.x0).fold(f64::INFINITY, f64::min))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    kept.into_iter().flatten().collect()
}

/// A run's bounding box.
fn run_box(run: &[Char]) -> (f64, f64, f64, f64) {
    (
        run.iter().map(|c| c.x0).fold(f64::INFINITY, f64::min),
        run.iter().map(|c| c.y0).fold(f64::INFINITY, f64::min),
        run.iter().map(|c| c.x1).fold(f64::NEG_INFINITY, f64::max),
        run.iter().map(|c| c.y1).fold(f64::NEG_INFINITY, f64::max),
    )
}

/// One line's text: its spans joined with single spaces (a span boundary is
/// already a word boundary — they split at wide gaps).
fn spans_text(spans: &[Span]) -> String {
    let mut text = String::new();
    for s in spans {
        let t = s.text.trim();
        if t.is_empty() {
            continue;
        }
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(t);
    }
    // " ," — a span that begins with punctuation hugs the previous word.
    text.replace(" ,", ",")
}

fn is_list_start(text: &str) -> bool {
    let t = text.trim_start();
    if t.is_empty() {
        return false;
    }
    let first = t.chars().next().unwrap();
    if "•◦▪▫‣⁃●○■□–—*✓✔➢➤►▶·".contains(first) {
        return true;
    }
    // "1.", "1)", "(1)", "a)", "A.", "iv." — an enumerator at the head.
    let bytes = t.as_bytes();
    let mut i = 0;
    if bytes.first() == Some(&b'(') {
        i = 1;
    }
    let digits_or_roman = bytes[i..]
        .iter()
        .take_while(|b| b.is_ascii_alphanumeric())
        .count();
    if digits_or_roman == 0 || digits_or_roman > 5 {
        return false;
    }
    let after = i + digits_or_roman;
    match bytes.get(after) {
        Some(b'.') | Some(b')') => {
            if i == 1 && bytes.get(after + 1) != Some(&b' ') {
                return false; // "(1)x" is not a list
            }
            if i == 0 && bytes.get(after + 1) != Some(&b' ') {
                // "3.14" is a number, not item 3.
                return !bytes.get(after + 1).is_some_and(|b| b.is_ascii_digit());
            }
            true
        }
        _ => false,
    }
}

/// papero's gutter finder: coverage of text height per 2pt strip; a valley
/// with text on both sides and nothing (but page-wide blocks) crossing it.
fn find_gutters(boxes: &[(f64, f64, f64, f64)]) -> Vec<(f64, f64)> {
    if boxes.len() < 3 {
        return Vec::new();
    }
    let left = boxes.iter().map(|b| b.0).fold(f64::INFINITY, f64::min);
    let right = boxes.iter().map(|b| b.2).fold(f64::NEG_INFINITY, f64::max);
    if right - left < 100.0 {
        return Vec::new();
    }
    let n = ((right - left) / GUTTER_STEP) as usize + 1;
    // A strip's coverage is the text HEIGHT crossing it — the box's own
    // height, not y1 (an early port summed the absolute y and every cover
    // number was meaningless). Boxes wider than ~3/4 of the page are the
    // full-width bands (a running head, a banner) and are not counted: a
    // single line across everything would lift every strip and hide the
    // gutters. The main column of a margin-note page sits just under this
    // line and IS counted — which is what makes its narrow, sparse
    // neighbour separable.
    let mut cover = vec![0.0f64; n];
    for &(x0, y0, x1, y1) in boxes {
        let wide = x1 - x0 > (right - left) * 0.78;
        let h = y1 - y0;
        let from = (((x0 - left) / GUTTER_STEP) as usize).min(n.saturating_sub(1));
        let to = (((x1 - left) / GUTTER_STEP) as usize).min(n);
        for k in from..to {
            if !wide {
                cover[k] += h;
            }
        }
    }
    let peak = cover.iter().cloned().fold(0.0, f64::max);
    if peak <= 0.0 {
        return Vec::new();
    }
    let mut out: Vec<(f64, f64)> = Vec::new();
    let mut k = 0usize;
    while k < n {
        if cover[k] > peak * 0.12 {
            k += 1;
            continue;
        }
        let mut j = k;
        while j < n && cover[j] <= peak * 0.12 {
            j += 1;
        }
        // The valley must have real text on both sides — a side column of a
        // margin-note page carries as little as a tenth of the body's
        // coverage, so the floor is a tenth of the peak (never under two
        // text lines, which keeps a stray glyph or a page number from
        // becoming a column). The old quarter-of-the-peak floor read every
        // sparse margin column as "no text" and its page as one column —
        // the margin's lines then merged into the body's mid-sentence.
        let before = cover[..k].iter().cloned().fold(0.0, f64::max);
        let after = cover[j..].iter().cloned().fold(0.0, f64::max);
        let width = (j - k) as f64 * GUTTER_STEP;
        let sides = before.min(after);
        if k > 0 && j < n && width >= MIN_GUTTER && sides >= peak * 0.10 && sides >= 24.0 {
            out.push((left + k as f64 * GUTTER_STEP, left + j as f64 * GUTTER_STEP));
        }
        k = j;
    }
    // At most three gutters: a two-column page is what a reader meets, a
    // three-column spread (a dense book table with a marker column, or a
    // slide split in three) still reads column by column. More than three
    // is never prose structure — and the table-band pass below works its
    // own strips within each column, so a grid's internal gutters do not
    // need to survive here.
    if out.len() > 3 {
        // Keep the three widest.
        out.sort_by(|a, b| {
            let wa = a.1 - a.0;
            let wb = b.1 - b.0;
            wb.partial_cmp(&wa).unwrap_or(std::cmp::Ordering::Equal)
        });
        out.truncate(3);
        out.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    }
    out
}

/// Column edges from gutters: (left-1, mid1), (mid1, mid2), (midn, right+1).
fn column_edges(boxes: &[(f64, f64, f64, f64)], gutters: &[(f64, f64)]) -> Vec<(f64, f64)> {
    if boxes.is_empty() {
        return Vec::new();
    }
    let left = boxes.iter().map(|b| b.0).fold(f64::INFINITY, f64::min);
    let right = boxes.iter().map(|b| b.2).fold(f64::NEG_INFINITY, f64::max);
    let mids: Vec<f64> = gutters.iter().map(|g| (g.0 + g.1) / 2.0).collect();
    let mut edges: Vec<f64> = Vec::new();
    edges.push(left - 1.0);
    edges.extend(mids);
    edges.push(right + 1.0);
    edges.windows(2).map(|w| (w[0], w[1])).collect()
}

/// The column a box's centre sits in, or None when it spans several.
fn col_of(s: &Span, cols: &[(f64, f64)]) -> Option<usize> {
    Some(col_of_box(s.x0, s.x1, cols))
}

fn col_of_box(x0: f64, x1: f64, cols: &[(f64, f64)]) -> usize {
    let mut best = 0usize;
    let mut best_overlap = 0.0f64;
    for (i, (lo, hi)) in cols.iter().enumerate() {
        let overlap = x1.min(*hi) - x0.max(*lo);
        if overlap > best_overlap {
            best_overlap = overlap;
            best = i;
        }
    }
    best
}

/// The unruled tables the books actually carry, read from text geometry
/// alone. A cause grid beside body prose, a scale box with a marker column:
/// their cells line up along vertical strips no glyph crosses, while the
/// prose above and below does cross — so the strip is clear only within a
/// y-band, and that band is the table. The band's cells read as rows of
/// cells joined "; " ("2; Short of breath when hurrying on the level or
/// walking up a slight hill"), each row its own block for the speech plan.
///
/// What is deliberately not attempted: rules and fills (the graphics state
/// neither glyph source tracks) — a ruled table whose cells would fuse
/// without them still needs them; a two-column band with nothing beside it
/// (a sub-column split of plain prose) is indistinguishable from a
/// two-column table and stays prose; rotated cells; a second stacked table
/// in the same column.
///
/// Returns the row spans and the indices (into `spans`) they consume.
fn table_rows_in_column(spans: &[&Span]) -> (Vec<Span>, Vec<usize>) {
    if spans.len() < 4 {
        return (Vec::new(), Vec::new());
    }
    let top = spans.iter().map(|s| s.y0).fold(f64::INFINITY, f64::min);
    let bottom = spans.iter().map(|s| s.y1).fold(f64::NEG_INFINITY, f64::max);
    // Candidate strips: right edges against left edges — where cells could
    // align. Every (x1, x0) pair this far apart is a candidate: the strip
    // need only be clear WITHIN the band, and a span starting inside it
    // (the prose above or below the table) is exactly what bounds the band.
    let mut x1s: Vec<f64> = spans.iter().map(|s| s.x1).collect();
    let mut xs0: Vec<f64> = spans.iter().map(|s| s.x0).collect();
    let dedup = |mut v: Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        v.dedup_by(|a, b| (*a - *b).abs() < 0.5);
        v
    };
    x1s = dedup(x1s);
    xs0 = dedup(xs0);
    let mut cands: Vec<(f64, f64)> = Vec::new();
    for a in &x1s {
        for b in &xs0 {
            if b - a >= MIN_GUTTER
                && !cands
                    .iter()
                    .any(|&(ca, cb)| (ca - *a).abs() < 0.5 && (cb - *b).abs() < 0.5)
            {
                cands.push((*a, *b));
            }
        }
    }
    // A strip's table = the tallest y-range it stays clear over, bounded by
    // the spans that cross it above and below (prose, a heading, a caption).
    // A strip with no crossing span on either side has no table: it is a
    // column edge or a margin, already handled by the page columns.
    let crossings_of = |a: f64, b: f64| -> Vec<&Span> {
        let mut c: Vec<&Span> = spans
            .iter()
            .copied()
            .filter(|s| s.x0 < b - CELL_EDGE_EPS && s.x1 > a + CELL_EDGE_EPS)
            .collect();
        c.sort_by(|x, y| x.y1.partial_cmp(&y.y1).unwrap_or(std::cmp::Ordering::Equal));
        c
    };
    // Accepted tables: (band y-range, strips). Strips whose bands overlap
    // one table heavily belong to it (a grid's second internal column).
    let mut tables: Vec<((f64, f64), Vec<(f64, f64)>)> = Vec::new();
    for &(a, b) in &cands {
        let crossings = crossings_of(a, b);
        if crossings.len() < 2 {
            continue;
        }
        // The tallest gap between consecutive crossings.
        let mut band: Option<(f64, f64)> = None;
        for w in crossings.windows(2) {
            let (lo, hi) = (w[0].y1, w[1].y0);
            if hi - lo > band.map(|(y0, y1)| y1 - y0).unwrap_or(0.0) {
                band = Some((lo, hi));
            }
        }
        let (y0, y1) = match band {
            Some(b) => b,
            None => continue,
        };
        // The band's cells: spans inside it that do not cross the strip.
        let cells: Vec<&Span> = spans
            .iter()
            .copied()
            .filter(|s| {
                s.y0 >= y0 - 0.5
                    && s.y1 <= y1 + 0.5
                    && !(s.x0 < b - CELL_EDGE_EPS && s.x1 > a + CELL_EDGE_EPS)
            })
            .collect();
        // Gates: real structure on both sides of the strip, enough rows.
        let left = cells.iter().filter(|s| s.x1 <= a + CELL_EDGE_EPS).count();
        let right = cells.iter().filter(|s| s.x0 >= b - CELL_EDGE_EPS).count();
        if left < 2 || right < 2 || cells.len() < 4 {
            continue;
        }
        // At least two visual rows among the cells.
        let mut sorted_by_y: Vec<&Span> = cells.clone();
        sorted_by_y.sort_by(|x, y| x.y0.partial_cmp(&y.y0).unwrap_or(std::cmp::Ordering::Equal));
        let rows_seen = 1 + sorted_by_y
            .windows(2)
            .filter(|w| {
                let small = w[0].size.min(w[1].size).max(4.0);
                w[1].y0 - w[0].y0 > small * 0.35
            })
            .count();
        if rows_seen < 2 {
            continue;
        }
        // The band must be a part of the column, not the whole column:
        // a strip clear from top to bottom is a margin, not a table's
        // gutter (a page number's margin, a figure's edge).
        if (y0 - top) < 1.0 && (bottom - y1) < 1.0 {
            continue;
        }
        match tables.iter_mut().find(|((ty0, ty1), _)| {
            let ov = y1.min(*ty1) - y0.max(*ty0);
            ov > 0.0 && ov / (y1 - y0).max(*ty1 - *ty0).max(1.0) >= 0.7
        }) {
            Some(((ty0, ty1), strips)) => {
                if !strips
                    .iter()
                    .any(|&(sa, sb)| (sa - a).abs() < 0.5 && (sb - b).abs() < 0.5)
                {
                    strips.push((a, b));
                }
                // The table is the union of its strips' bands — and every
                // strip must stay clear across the union: a strip some cell
                // crosses was bounded by that cell, not by the table, and
                // is not a boundary of it.
                *ty0 = (*ty0).min(y0);
                *ty1 = (*ty1).max(y1);
                strips.retain(|&(sa, sb)| {
                    !spans.iter().any(|s| {
                        s.y0 >= *ty0 - 0.5
                            && s.y1 <= *ty1 + 0.5
                            && s.x0 < sb - CELL_EDGE_EPS
                            && s.x1 > sa + CELL_EDGE_EPS
                    })
                });
            }
            None => tables.push(((y0, y1), vec![(a, b)])),
        }
    }
    tables.retain(|(_, strips)| !strips.is_empty());
    // One table per column: the one with the most cells (then most strips).
    let chosen = tables.into_iter().max_by_key(|((y0, y1), strs)| {
        let cells = spans
            .iter()
            .filter(|s| s.y0 >= y0 - 0.5 && s.y1 <= y1 + 0.5)
            .count();
        cells * 1000 + strs.len()
    });
    let Some(((ty0, ty1), strips)) = chosen else {
        return (Vec::new(), Vec::new());
    };
    let mut ss = strips;
    ss.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal));
    let n_cols = ss.len() + 1;
    let subcol_of = |s: &Span| -> usize {
        for (i, (a, b)) in ss.iter().enumerate() {
            if s.x1 <= *a + CELL_EDGE_EPS {
                return i;
            }
            if s.x0 >= *b - CELL_EDGE_EPS {
                continue;
            }
            return i + 1;
        }
        n_cols - 1
    };
    // The band's cells: everything inside the y-range that crosses none of
    // the strips (a cell of a grid belongs to exactly one sub-column).
    let crosses_strip =
        |s: &Span| ss.iter().any(|&(a, b)| s.x0 < b - CELL_EDGE_EPS && s.x1 > a + CELL_EDGE_EPS);
    let cells: Vec<&Span> = spans
        .iter()
        .copied()
        .filter(|s| s.y0 >= ty0 - 0.5 && s.y1 <= ty1 + 0.5 && !crosses_strip(s))
        .collect();
    if cells.len() < 4 {
        return (Vec::new(), Vec::new());
    }
    // Cell lines: a sub-column's spans that share a baseline are one cell
    // line (a split word, a dash between two runs).
    let mut lines_by_col: Vec<Vec<Vec<&Span>>> = vec![Vec::new(); n_cols];
    {
        let mut per_col: Vec<Vec<&Span>> = vec![Vec::new(); n_cols];
        for s in &cells {
            per_col[subcol_of(s)].push(s);
        }
        for (c, col) in per_col.iter_mut().enumerate() {
            col.sort_by(|x, y| {
                x.y0.partial_cmp(&y.y0).unwrap_or(std::cmp::Ordering::Equal).then(
                    x.x0.partial_cmp(&y.x0).unwrap_or(std::cmp::Ordering::Equal),
                )
            });
            for s in col.drain(..) {
                match lines_by_col[c].last_mut() {
                    Some(line)
                        if line.last().is_some_and(|l| {
                            let small = l.size.min(s.size).max(4.0);
                            s.y0 - l.y0 <= small * 0.35 && s.y0 < l.y1 - small * 0.2
                        }) =>
                    {
                        line.push(s);
                    }
                    _ => lines_by_col[c].push(vec![s]),
                }
            }
        }
    }
    // Rows: anchor on the top-most unassigned cell line, then take each
    // other sub-column's line that overlaps it most.
    struct CellLine {
        col: usize,
        text: String,
        y0: f64,
        y1: f64,
        size: f64,
        x0: f64,
        x1: f64,
        taken: bool,
    }
    let mut all_lines: Vec<CellLine> = Vec::new();
    for (c, lines) in lines_by_col.iter().enumerate() {
        for line in lines {
            let owned: Vec<Span> = line.iter().map(|s| (*s).clone()).collect();
            let text = spans_text(&owned);
            if text.trim().is_empty() {
                continue;
            }
            all_lines.push(CellLine {
                col: c,
                text,
                y0: line.iter().map(|s| s.y0).fold(f64::INFINITY, f64::min),
                y1: line.iter().map(|s| s.y1).fold(f64::NEG_INFINITY, f64::max),
                size: line.iter().map(|s| s.size).fold(0.0, f64::max),
                x0: line.iter().map(|s| s.x0).fold(f64::INFINITY, f64::min),
                x1: line.iter().map(|s| s.x1).fold(f64::NEG_INFINITY, f64::max),
                taken: false,
            });
        }
    }
    all_lines.sort_by(|x, y| {
        x.y0
            .partial_cmp(&y.y0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(x.col.cmp(&y.col))
    });
    let mut rows: Vec<(Vec<String>, f64, f64, f64, f64, f64)> = Vec::new(); // cells, y0, y1, x0, x1, size
    for ai in 0..all_lines.len() {
        if all_lines[ai].taken {
            continue;
        }
        all_lines[ai].taken = true;
        let (acol, mut y0, mut y1, mut x0, mut x1, mut size) = {
            let a = &all_lines[ai];
            (a.col, a.y0, a.y1, a.x0, a.x1, a.size)
        };
        let mut cells: Vec<String> = vec![String::new(); n_cols];
        cells[acol] = all_lines[ai].text.clone();
        for c in 0..n_cols {
            if c == acol {
                continue;
            }
            let mut best: Option<usize> = None;
            let mut best_ov = -1.0f64;
            for (li, l) in all_lines.iter().enumerate() {
                if l.taken || l.col != c {
                    continue;
                }
                let ov = y1.min(l.y1) - y0.max(l.y0);
                if ov >= 0.35 * size.min(l.size).max(4.0) && ov > best_ov {
                    best_ov = ov;
                    best = Some(li);
                }
            }
            if let Some(li) = best {
                let l = &all_lines[li];
                cells[c] = l.text.clone();
                y0 = y0.min(l.y0);
                y1 = y1.max(l.y1);
                x0 = x0.min(l.x0);
                x1 = x1.max(l.x1);
                size = size.max(l.size);
                all_lines[li].taken = true;
            }
        }
        rows.push((cells, y0, y1, x0, x1, size));
    }
    if rows.len() < 2 {
        return (Vec::new(), Vec::new());
    }
    // A row that opens with an empty marker column is its predecessor's
    // continuation: a wrapped cell ("slight hill" under "Short of breath
    // … up a"), a wrapped list item ("taneous, think of…" under "• How did
    // the breathlessness start?…"), the next clue of a grid cell. Merge it
    // into the row above, whose marker cell (a bullet, a digit, a cause)
    // is what the continuation hangs from. A row that opens with text of
    // its own is a new row.
    let mut merged: Vec<(Vec<String>, f64, f64, f64, f64, f64)> = Vec::new();
    for (cells, y0, y1, x0, x1, size) in rows {
        if let Some((last_cells, _, ly1, lx0, lx1, lsize)) = merged.last_mut() {
            let opens_empty = cells.first().map_or(true, |c| c.trim().is_empty());
            if opens_empty {
                for (c, cell) in cells.into_iter().enumerate() {
                    if cell.trim().is_empty() {
                        continue;
                    }
                    let target = &mut last_cells[c];
                    if target.ends_with('-')
                        && target.len() > 1
                        && target.as_bytes()[target.len() - 2].is_ascii_alphabetic()
                        && cell.chars().next().is_some_and(|ch| ch.is_lowercase())
                    {
                        target.pop();
                        target.push_str(cell.trim());
                    } else if target.is_empty() {
                        *target = cell.trim().to_string();
                    } else {
                        target.push(' ');
                        target.push_str(cell.trim());
                    }
                }
                *ly1 = (*ly1).max(y1);
                *lx0 = (*lx0).min(x0);
                *lx1 = (*lx1).max(x1);
                *lsize = (*lsize).max(size);
                continue;
            }
        }
        merged.push((cells, y0, y1, x0, x1, size));
    }
    // The rows become pre-joined spans in the reading order, each its own
    // block.
    let used: Vec<usize> = (0..spans.len())
        .filter(|&i| spans[i].y0 >= ty0 - 0.5 && spans[i].y1 <= ty1 + 0.5)
        .collect();
    if used.len() < 4 {
        return (Vec::new(), Vec::new());
    }
    let out = merged
        .into_iter()
        .map(|(cells, y0, y1, x0, x1, size)| Span {
            text: cells
                .iter()
                .map(|c| c.trim())
                .filter(|c| !c.is_empty())
                .collect::<Vec<_>>()
                .join("; "),
            x0,
            y0,
            x1,
            y1,
            size: size.max(4.0),
            bold: false,
            mono: false,
            row: true,
        })
        .filter(|s| !s.text.trim().is_empty())
        .collect();
    (out, used)
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::Document;

    /// A one-page PDF with a simple content stream, built by hand.
    fn fixture(stream: &str) -> Document {
        let objects = [
            "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_string(),
            "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_string(),
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>\nendobj\n".to_string(),
            format!("4 0 obj\n<< /Length {} >>\nstream\n{}\nendstream\nendobj\n", stream.len(), stream),
            "5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n".to_string(),
        ];
        let mut body = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for o in &objects {
            offsets.push(body.len());
            body.push_str(o);
        }
        let xref_at = body.len();
        body.push_str(&format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1));
        for off in &offsets {
            body.push_str(&format!("{off:010} 00000 n \n"));
        }
        body.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n",
            objects.len() + 1
        ));
        Document::parse(body.as_bytes()).expect("fixture parses")
    }

    #[test]
    fn one_tj_paints_one_block() {
        let doc = fixture("BT /F1 14 Tf 72 720 Td (Hello world.) Tj ET");
        let pages = extract(&doc);
        assert_eq!(pages.len(), 1);
        assert!(pages[0].text.contains("Hello world."), "got {:?}", pages[0].text);
    }

    #[test]
    fn two_lines_become_two_paragraphs() {
        let doc = fixture("BT /F1 12 Tf 72 720 Td (First line.) Tj 0 -24 Td (Second line.) Tj ET");
        let pages = extract(&doc);
        let text = &pages[0].text;
        assert!(text.contains("First line.") && text.contains("Second line."), "{text:?}");
    }

    #[test]
    fn a_td_chain_positions_glyphs_in_order() {
        // Per-glyph Td: the writer's common idiom. The chars must come out
        // in paint order with increasing x.
        let doc = fixture("BT /F1 12 Tf 100 700 Td (a) Tj 12 0 Td (b) Tj 12 0 Td (c) Tj ET");
        let pages = extract(&doc);
        // The advance steps are wider than the estimate, so the layout
        // reads them as word gaps — the point is the ORDER survives.
        let compact: String = pages[0].text.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("abc"), "{:?}", pages[0].text);
    }

    #[test]
    fn word_gaps_become_spaces() {
        // Words painted with no space code, separated only by a pen jump:
        // the box gap says "space".
        let doc = fixture("BT /F1 12 Tf 72 720 Td (word) Tj 36 0 Td (next) Tj ET");
        let pages = extract(&doc);
        assert!(pages[0].text.contains("word next"), "{:?}", pages[0].text);
    }

    #[test]
    fn tj_kerns_move_the_pen() {
        // A TJ array with kerning: the numbers move the pen between strings.
        let doc = fixture("BT /F1 12 Tf 72 720 Td [(W) 120 (ord)] TJ ET");
        let pages = extract(&doc);
        assert!(pages[0].text.contains("W"), "{:?}", pages[0].text);
    }

    #[test]
    fn cm_moves_text_to_the_right_column() {
        // A content stream that shifts a text block with cm: the block's
        // boxes must land right of the unshifted one.
        let doc = fixture(
            "q 1 0 0 1 300 0 cm BT /F1 12 Tf 72 720 Td (right) Tj ET Q\nBT /F1 12 Tf 72 720 Td (left) Tj ET",
        );
        let pages = extract(&doc);
        let blocks = &pages[0].blocks;
        assert!(blocks
            .iter()
            .any(|b| b.text.contains("right") && b.text.contains("left")),
            "{:?}", pages[0].text);
    }

    #[test]
    fn is_list_start_knows_enumerators() {
        assert!(is_list_start("• bullet"));
        assert!(is_list_start("1. first"));
        assert!(is_list_start("a) option"));
        assert!(is_list_start("(3) item"));
        assert!(!is_list_start("3.14 is pi"));
        assert!(!is_list_start("plain paragraph text"));
    }

    #[test]
    fn empty_pages_come_back_empty() {
        let doc = fixture("1 0 0 1 0 0 cm 0 0 1 rg 10 10 50 50 re f");
        let pages = extract(&doc);
        assert_eq!(pages.len(), 1);
        assert!(pages[0].text.is_empty());
    }

    #[test]
    fn two_overlapping_runs_do_not_interleave() {
        // An OCR text layer paints its word gaps as box overlaps: run 2 starts
        // before run 1's estimate ends. Glyph x-sorting interleaves the runs
        // mid-word; run order keeps each run whole: "the 4 year".
        let doc = fixture(
            "BT /F1 12 Tf 72 720 Td [(t) 2 (he 4)] TJ ET \
             BT /F1 12 Tf 140 720 Td [( ye) 2 (ar)] TJ ET",
        );
        let pages = extract(&doc);
        assert!(
            pages[0].text.contains("the 4") && pages[0].text.contains("year"),
            "{:?}",
            pages[0].text
        );
    }

    #[test]
    fn a_shadow_copy_of_a_run_collapses() {
        // A slide title painted twice over itself (outline under fill): one
        // "Lecture", not "LectureLecture".
        let doc = fixture(
            "BT /F1 54 Tf 292 500 Td (Lecture) Tj ET \
             BT /F1 55 Tf 295 500 Td (Lecture) Tj ET",
        );
        let pages = extract(&doc);
        assert_eq!(pages[0].text.trim(), "Lecture", "{:?}", pages[0].text);
    }

    #[test]
    fn a_painted_space_survives_overlapping_boxes() {
        // The stream painted a space between two words whose boxes overlap
        // (the pen estimates disagree with the writer's). The flag on the
        // glyph that followed the space keeps the word break: "signs and",
        // never "signsand".
        let doc = fixture("BT /F1 12 Tf 72 720 Td [(s) 2 (igns )(a) 2 (nd)] TJ ET");
        let pages = extract(&doc);
        assert!(pages[0].text.contains("signs and"), "{:?}", pages[0].text);
    }

    // --- ToUnicode CMaps ----------------------------------------------------

    /// A one-page PDF with a Type0 font (`/Encoding /Identity-H`, two-byte
    /// codes) whose `/ToUnicode` is `cmap`: the fixture for every CMap form
    /// real producers emit.
    fn type0_fixture(cmap: &str, content: &str) -> Document {
        let objects = [
            "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_string(),
            "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_string(),
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>\nendobj\n".to_string(),
            format!("4 0 obj\n<< /Length {} >>\nstream\n{}\nendstream\nendobj\n", content.len(), content),
            "5 0 obj\n<< /Type /Font /Subtype /Type0 /BaseFont /Test /Encoding /Identity-H /DescendantFonts [6 0 R] /ToUnicode 7 0 R >>\nendobj\n".to_string(),
            "6 0 obj\n<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Test /CIDSystemInfo << /Adobe /Identity >> /DW 600 /W [1 4 600] >>\nendobj\n".to_string(),
            format!("7 0 obj\n<< /Length {} >>\nstream\n{}\nendstream\nendobj\n", cmap.len(), cmap),
        ];
        let mut body = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for o in &objects {
            offsets.push(body.len());
            body.push_str(o);
        }
        let xref_at = body.len();
        body.push_str(&format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1));
        for off in &offsets {
            body.push_str(&format!("{off:010} 00000 n \n"));
        }
        body.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n",
            objects.len() + 1
        ));
        Document::parse(body.as_bytes()).expect("type0 fixture parses")
    }

    const CMAP_HEAD: &str = "/CIDInit /ProcSet findresource begin\nbegincmap\n";

    #[test]
    fn a_ligature_dst_of_two_chars_maps_to_two_chars() {
        // <00660069> is "fi" in UTF-16BE — read as a surrogate pair, it
        // truncated to "f" and every "fifth" came out "ffth".
        let cmap = format!(
            "{CMAP_HEAD}4 beginbfchar\n<0001> <00660069>\n<0002> <0066>\n<0003> <0074>\n<0004> <0068>\nendbfchar\nendcmap\nend\n"
        );
        let content = "BT /F1 12 Tf 72 720 Td <0001> Tj <0002> Tj <0003> Tj <0004> Tj ET";
        let doc = type0_fixture(&cmap, content);
        let text = &extract(&doc)[0].text;
        assert!(text.contains("fifth"), "{text:?}");
        assert!(!text.contains("ffth"), "{text:?}");
    }

    #[test]
    fn a_ligature_single_char_dst_still_expands() {
        // <FB01> maps to the ﬁ character; the glyph's normalization expands
        // it into "fi" — and truncating that expansion at its first char is
        // the same "ffth" bug wearing a different CMap. A plain "f" dst is
        // the control.
        let cmap = format!("{CMAP_HEAD}1 beginbfchar\n<0001> <FB01>\nendbfchar\nendcmap\nend\n");
        let doc = type0_fixture(&cmap, "BT /F1 12 Tf 72 720 Td <0001> Tj ET");
        assert_eq!(extract(&doc)[0].text.trim(), "fi");
        let cmap = format!("{CMAP_HEAD}1 beginbfchar\n<0001> <0066>\nendbfchar\nendcmap\nend\n");
        let doc = type0_fixture(&cmap, "BT /F1 12 Tf 72 720 Td <0001> Tj ET");
        assert_eq!(extract(&doc)[0].text.trim(), "f");
    }

    #[test]
    fn a_bfrange_with_one_destination_per_code_maps_each() {
        // `beginbfrange <01> <03> [<0041> <0042> <0043>]` — the array form
        // many CMap writers emit instead of the arithmetic one.
        let cmap = format!(
            "{CMAP_HEAD}1 beginbfrange\n<0001> <0003> [<0041> <0042> <0043>]\nendbfrange\nendcmap\nend\n"
        );
        let content = "BT /F1 12 Tf 72 720 Td <010203> Tj ET";
        let doc = type0_fixture(&cmap, content);
        assert_eq!(extract(&doc)[0].text.trim(), "ABC");
    }

    #[test]
    fn a_bfrange_arithmetic_form_still_advances_the_destination() {
        let cmap = format!("{CMAP_HEAD}1 beginbfrange\n<0001> <0003> <0041>\nendbfrange\nendcmap\nend\n");
        let content = "BT /F1 12 Tf 72 720 Td <010203> Tj ET";
        let doc = type0_fixture(&cmap, content);
        assert_eq!(extract(&doc)[0].text.trim(), "ABC");
    }

    // --- the writer's word gaps ----------------------------------------------

    #[test]
    fn a_wide_tj_displacement_opens_a_word_gap() {
        // A negative TJ number pushes the pen right; one this wide is the
        // space glyph the writer omitted. The gap itself hides inside the
        // glyph's estimated box (the pen moved less than the estimate), so
        // only the displacement says "space".
        let doc = fixture("BT /F1 12 Tf 72 720 Td [(s) -160 (igns)] TJ ET");
        let pages = extract(&doc);
        assert!(pages[0].text.contains("s igns"), "{:?}", pages[0].text);
    }

    #[test]
    fn a_tiny_tj_displacement_is_still_kerning() {
        let doc = fixture("BT /F1 12 Tf 72 720 Td [(W) -80 (ord)] TJ ET");
        let pages = extract(&doc);
        assert!(pages[0].text.contains("Word"), "{:?}", pages[0].text);
    }

    #[test]
    fn character_spacing_is_text_space_and_carries_word_gaps() {
        // Tf 1 with the real size in Tm — bookbaz's producer — writing word
        // gaps as character spacing: Tc rides the text matrix, and the ink
        // box excludes it, so the gap the writer drew is a gap the layout
        // sees: ") and", never ")and".
        let content = "BT /F2 1 Tf 0.5 Tc 7.9702 0 0 7.9702 0 0 Tm 31.035 74.062 TD [(\\)a) 500 (n) 500 (d)] TJ ET";
        let objects = [
            "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_string(),
            "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_string(),
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F2 5 0 R >> >> >>\nendobj\n".to_string(),
            format!("4 0 obj\n<< /Length {} >>\nstream\n{}\nendstream\nendobj\n", content.len(), content),
            "5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n".to_string(),
        ];
        let mut body = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for o in &objects {
            offsets.push(body.len());
            body.push_str(o);
        }
        let xref_at = body.len();
        body.push_str(&format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1));
        for off in &offsets {
            body.push_str(&format!("{off:010} 00000 n \n"));
        }
        body.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n",
            objects.len() + 1
        ));
        let doc = Document::parse(body.as_bytes()).expect("parses");
        let text = &extract(&doc)[0].text;
        assert!(text.contains(") and"), "{text:?}");
        assert!(!text.contains(")and"), "{text:?}");
        assert!(text.contains("and"), "{text:?}");
    }

    // --- reading order -------------------------------------------------------

    #[test]
    fn a_sparse_margin_column_reads_before_the_body() {
        // A margin note column carries a fraction of the body's coverage:
        // the old gutter thresholds read it as "no text" and its lines
        // merged into the body's mid-sentence. Three note lines, six body
        // lines — the note's column must read as its own block, before the
        // body.
        let content = "BT /F1 12 Tf 50 700 Td (margin note one) Tj 0 -14 Td (margin note two) Tj 0 -14 Td (margin note three) Tj ET \
             BT /F1 12 Tf 300 700 Td (body alpha line) Tj 0 -14 Td (body beta line) Tj 0 -14 Td (body gamma line) Tj 0 -14 Td (body delta line) Tj 0 -14 Td (body epsilon line) Tj 0 -14 Td (body zeta line) Tj ET";
        let doc = fixture(content);
        let text = &extract(&doc)[0].text;
        let note_pos = text.find("margin note three").expect("note block read whole");
        let body_pos = text.find("body alpha line").expect("body present");
        assert!(note_pos < body_pos, "margin column reads first: {text:?}");
        // No body line spliced into the note's middle.
        let note_end = text.find("margin note three").unwrap() + "margin note three".len();
        assert!(
            !text[note_end..body_pos].contains("body"),
            "note block stays whole: {text:?}"
        );
    }

    #[test]
    fn two_columns_read_column_by_column() {
        // The row-wise band walk read a two-column page as
        // "left1 right1 left2 right2…" — rubble. Columns read whole.
        let content = "BT /F1 12 Tf 72 700 Td (left alpha) Tj 0 -14 Td (left beta) Tj 0 -14 Td (left gamma) Tj 0 -14 Td (left delta) Tj 0 -14 Td (left epsilon) Tj 0 -14 Td (left zeta) Tj ET \
             BT /F1 12 Tf 400 700 Td (right alpha) Tj 0 -14 Td (right beta) Tj 0 -14 Td (right gamma) Tj 0 -14 Td (right delta) Tj 0 -14 Td (right epsilon) Tj 0 -14 Td (right zeta) Tj ET";
        let doc = fixture(content);
        let text = &extract(&doc)[0].text;
        let l_first = text.find("left alpha").unwrap();
        let l_last = text.find("left zeta").unwrap();
        let r_first = text.find("right alpha").unwrap();
        assert!(l_last < r_first, "left column reads whole, then right: {text:?}");
        assert!(l_first < r_first);
    }

    #[test]
    fn a_full_width_heading_reads_before_the_columns_it_crosses() {
        let heading = "The Long Chapter Heading That Spans Both Columns";
        let content = format!(
            "BT /F1 14 Tf 72 740 Td ({heading}) Tj ET \
             BT /F1 12 Tf 72 700 Td (left alpha) Tj 0 -14 Td (left beta) Tj 0 -14 Td (left gamma) Tj 0 -14 Td (left delta) Tj ET \
             BT /F1 12 Tf 400 700 Td (right alpha) Tj 0 -14 Td (right beta) Tj 0 -14 Td (right gamma) Tj 0 -14 Td (right delta) Tj ET"
        );
        let doc = fixture(&content);
        let text = &extract(&doc)[0].text;
        let h = text.find(heading).expect("heading present");
        let l = text.find("left alpha").unwrap();
        let r = text.find("right alpha").unwrap();
        assert!(h < l && h < r, "heading reads first: {text:?}");
        assert!(text.find("left delta").unwrap() < r, "left column whole before right: {text:?}");
    }

    #[test]
    fn a_single_column_page_still_reads_top_down() {
        // One column, no gutter: the plain order — and no column machinery
        // splitting a narrow page in two.
        let content = "BT /F1 12 Tf 72 700 Td (first paragraph line) Tj 0 -14 Td (second paragraph line) Tj 0 -28 Td (third paragraph line) Tj ET";
        let doc = fixture(content);
        let text = &extract(&doc)[0].text;
        let a = text.find("first paragraph").unwrap();
        let b = text.find("second paragraph").unwrap();
        let c = text.find("third paragraph").unwrap();
        assert!(a < b && b < c, "{text:?}");
    }

    // --- table bands: the unruled tables the real books carry ------------

    #[test]
    fn a_scale_box_reads_one_row_per_grade() {
        // bookbaz p101's MRC breathlessness scale: a marker column (grade
        // digits) beside a text column, wrapped cells, and body prose
        // beside the box at a 4 pt stagger. The stagger chained row,
        // continuation and body prose into one y-chain, and the
        // x-interleaved runs fused cell fragments into the words
        // ("Shortslight of hill breath"). Now: the body line splits off at
        // its x-jump, and the box reads as rows of cells — grade first,
        // the whole cell text after it.
        let content = "\
             BT /F1 8 Tf 51 724 Td (Grade) Tj ET \
             BT /F1 8 Tf 81 724 Td (Degree of breathlessness) Tj ET \
             BT /F1 8 Tf 51 712 Td (1) Tj ET \
             BT /F1 8 Tf 81 712 Td (Not troubled by breathlessness) Tj ET \
             BT /F1 8 Tf 51 700 Td (2) Tj ET \
             BT /F1 8 Tf 81 700 Td (Short of breath when hurrying) Tj ET \
             BT /F1 8 Tf 81 696 Td (slight hill) Tj ET \
             BT /F1 8 Tf 276 696 Td (bronchiectasis or cardiac failure) Tj ET \
             BT /F1 8 Tf 276 708 Td (right column line one) Tj 0 -12 Td (right column line two) Tj 0 -12 Td (right column line three) Tj 0 -12 Td (right column line four) Tj 0 -12 Td (right column line five) Tj ET \
             BT /F1 8 Tf 51 688 Td (3) Tj ET \
             BT /F1 8 Tf 81 688 Td (Walks slower than most people) Tj ET \
             BT /F1 8 Tf 81 684 Td (or so, or stops after) Tj ET \
             BT /F1 8 Tf 51 660 Td (Used with the permission) Tj ET";
        let doc = fixture(content);
        let text = &extract(&doc)[0].text;
        assert!(
            text.contains("1; Not troubled by breathlessness"),
            "grade 1 reads as a row: {text:?}"
        );
        assert!(
            text.contains("2; Short of breath when hurrying slight hill"),
            "the wrapped cell hangs from its grade: {text:?}"
        );
        assert!(
            text.contains("3; Walks slower than most people or so, or stops after"),
            "grade 3 reads as a row: {text:?}"
        );
        assert!(
            !text.contains("Shortslight") && !text.contains("of hill breath"),
            "no fused cell fragments: {text:?}"
        );
        // The body prose beside the box reads its own column, after the
        // box — never spliced into a row.
        let body = text.find("bronchiectasis or cardiac failure").expect("body present");
        let last_row = text.find("3; Walks slower").unwrap();
        assert!(last_row < body, "prose reads after the box: {text:?}");
    }

    #[test]
    fn a_grid_beside_prose_reads_rows_of_cells() {
        // bookbaz p103's causes grid: two cell columns inside the left
        // page column, body prose in the right page column, lines
        // staggered so row, wrap and prose chained into one y-chain and
        // the grid read as interleaved rubble. Now each grid row reads
        // "cause; clues".
        let content = "\
             BT /F1 8 Tf 51 724 Td (5.3 Causes and clues in the history) Tj ET \
             BT /F1 8 Tf 51 712 Td (Asthma) Tj ET \
             BT /F1 8 Tf 145 712 Td (Affects children) Tj ET \
             BT /F1 8 Tf 145 707.5 Td (worse at night) Tj ET \
             BT /F1 8 Tf 51 700 Td (Cancer) Tj ET \
             BT /F1 8 Tf 145 700 Td (Breathless at rest) Tj ET \
             BT /F1 8 Tf 51 688 Td (Reflux) Tj ET \
             BT /F1 8 Tf 145 688 Td (Heartburn after meals) Tj ET \
             BT /F1 8 Tf 51 660 Td (Prose below the grid spans the column) Tj ET \
             BT /F1 8 Tf 290 712 Td (right alpha) Tj 0 -12 Td (right beta) Tj 0 -12 Td (right gamma) Tj 0 -12 Td (right delta) Tj 0 -12 Td (right epsilon) Tj 0 -12 Td (right zeta) Tj ET";
        let doc = fixture(content);
        let text = &extract(&doc)[0].text;
        assert!(
            text.contains("Asthma; Affects children worse at night"),
            "the wrapped clue hangs from its cause: {text:?}"
        );
        assert!(text.contains("Cancer; Breathless at rest"), "{text:?}");
        assert!(text.contains("Reflux; Heartburn after meals"), "{text:?}");
        // The grid reads before the prose column beside it.
        let grid = text.find("Reflux; Heartburn after meals").unwrap();
        let prose = text.find("right alpha").unwrap();
        assert!(grid < prose, "grid reads before the prose column: {text:?}");
    }

    #[test]
    fn a_list_wrap_hangs_from_its_own_marker() {
        // A bulleted list beside nothing: each item's wrapped lines merge
        // into the item (hyphen undo included), and the NEXT item's marker
        // starts a new row — wraps never spill across items.
        let content = "\
             BT /F1 8 Tf 51 730 Td (Prose above the list spans the column) Tj ET \
             BT /F1 8 Tf 51 718 Td (*) Tj ET \
             BT /F1 8 Tf 68 718 Td (How did the breathlessness start instan-) Tj ET \
             BT /F1 8 Tf 68 713.5 Td (taneous, think of pneumothorax) Tj ET \
             BT /F1 8 Tf 51 702 Td (*) Tj ET \
             BT /F1 8 Tf 68 702 Td (Wake a sleeping patient with breath-) Tj ET \
             BT /F1 8 Tf 68 697.5 Td (lessness at night) Tj ET \
             BT /F1 8 Tf 51 680 Td (Prose below the list spans the column) Tj ET";
        let doc = fixture(content);
        let text = &extract(&doc)[0].text;
        assert!(
            text.contains("start instantaneous, think of pneumothorax"),
            "item one reads whole, hyphen undone: {text:?}"
        );
        assert!(
            text.contains("with breathlessness at night"),
            "item two reads whole, hyphen undone: {text:?}"
        );
        let one = text.find("*; How did the breathlessness").expect("item one row");
        let two = text.find("*; Wake a sleeping patient").expect("item two row");
        assert!(one < two, "items read in order: {text:?}");
    }

    #[test]
    fn a_three_column_page_reads_column_by_column() {
        // Two gutters on one page: a three-column spread reads whole
        // columns left to right, not the widest gutter with the other two
        // columns interleaved inside it.
        let content = "\
             BT /F1 12 Tf 50 700 Td (left alpha) Tj 0 -14 Td (left beta) Tj 0 -14 Td (left gamma) Tj 0 -14 Td (left delta) Tj 0 -14 Td (left epsilon) Tj 0 -14 Td (left zeta) Tj ET \
             BT /F1 12 Tf 250 700 Td (mid alpha) Tj 0 -14 Td (mid beta) Tj 0 -14 Td (mid gamma) Tj 0 -14 Td (mid delta) Tj 0 -14 Td (mid epsilon) Tj 0 -14 Td (mid zeta) Tj ET \
             BT /F1 12 Tf 450 700 Td (right alpha) Tj 0 -14 Td (right beta) Tj 0 -14 Td (right gamma) Tj 0 -14 Td (right delta) Tj 0 -14 Td (right epsilon) Tj 0 -14 Td (right zeta) Tj ET";
        let doc = fixture(content);
        let text = &extract(&doc)[0].text;
        let l = text.find("left alpha").unwrap()..text.find("left zeta").unwrap();
        let m = text.find("mid alpha").unwrap()..text.find("mid zeta").unwrap();
        let r = text.find("right alpha").unwrap();
        assert!(l.end < m.start && m.end < r, "whole columns left to right: {text:?}");
    }
}
