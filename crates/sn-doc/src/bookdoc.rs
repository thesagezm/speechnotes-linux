//! The shared shape every headless book format reduces to.
//!
//! EPUB is an archive of XHTML the reader serves verbatim; PDF is pages; a
//! slide deck is slides. The formats this module serves — the Mobipocket/
//! Kindle family, FictionBook, RTF, HTML/HTMLZ and plain text — have no
//! container the reader can serve as-is, so each parser reduces its file to
//! one [`BookDoc`]: metadata, a cover, chapters of XHTML, a table of
//! contents and the images those chapters reference. Nothing here is EPUB
//! and nothing here becomes EPUB: the chapters render straight into the
//! reader's web engine, the speech path extracts them with the ordinary
//! [`crate::chapter`] pass, and the file on disk stays the file the user
//! imported (`original.mobi`, `original.fb2`, … — never an `original.epub`).
//!
//! The chapter XHTML is deliberately plain — `h1..h6`, `p`, `em`, `strong`,
//! `blockquote`, `table`, `img` — the vocabulary [`crate::chapter::extract`]
//! already reads for structure and speech, and the vocabulary the reader's
//! appearance layer styles without help.
//!
//! Ported in part from the sibling speechnotes-ios parsers (DocumentBook
//!.swift and its readers), which solve the same reduction for the same
//! family — the iOS app then *converts* the result to an .epub file; this
//! crate stops at the model and keeps the original file.

/// Why a headless book refused to parse. The import path shows this to the
/// user, so the messages name the file's own failure, not the parser's.
#[derive(Debug, thiserror::Error)]
pub enum BookDocError {
    #[error("{0}")]
    Malformed(String),
    /// The file parses as its format but the format's content is out of
    /// reach: a DRM-locked book, a variant no reader decodes. Distinct from
    /// [`BookDocError::Malformed`] because the import path tells the user
    /// something different — the file is fine, this app just can't read it.
    #[error("{0}")]
    Unsupported(String),
    #[error("the text is {k}k characters — beyond the {m}M cap")]
    TooLarge { k: usize, m: usize },
}

/// One image a book carries: its cover, or an inline illustration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookImage {
    /// The name the chapter XHTML references it by, `img/`-prefixed
    /// (`img/cover`, `img/000123`). A parser invents these; the only rule
    /// is that they are unique within one [`BookDoc`] and safe in a URL
    /// path (letters, digits, `-`, `_`, `.`).
    pub name: String,
    /// `image/jpeg` or `image/png` — the two the shelf and the web engine
    /// need, and the two every source format carries.
    pub mime: String,
    pub data: Vec<u8>,
}

/// One chapter: the XHTML the reader serves and the speech path extracts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BookChapter {
    /// The chapter's own title, when the format carried one (an FB2
    /// `<section><title>`, a plain-text heading line). The manifest's TOC
    /// falls back to this when the format has no separate TOC.
    pub title: Option<String>,
    /// The chapter's body XHTML. Not a full document — no `<html>`/`<head>`
    /// — the body the chapter page wraps.
    pub html: String,
}

/// One table-of-contents row: a label and the chapter it opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookToc {
    pub label: String,
    /// Index into [`BookDoc::chapters`].
    pub chapter_index: usize,
    /// Nesting depth, 0 = top level, carried to the manifest so the
    /// contents list indents the way the book's own did.
    pub depth: usize,
}

/// A whole headless book, parsed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BookDoc {
    pub title: Option<String>,
    pub author: Option<String>,
    pub language: Option<String>,
    /// The cover, when the format carries one — what the shelf shows.
    pub cover: Option<BookImage>,
    /// The reading order. An empty list is a refused import, the same rule
    /// an EPUB with no spine answers to.
    pub chapters: Vec<BookChapter>,
    /// The book's own contents, when the format carries one (a MOBI's
    /// CNCX, an FB2's nested sections). A format with none — plain text —
    /// leaves this empty and the manifest derives its TOC from the
    /// chapters' titles.
    pub toc: Vec<BookToc>,
    /// Every image the chapters reference, [`BookImage::name`] keyed.
    pub images: Vec<BookImage>,
}

impl BookDoc {
    /// The spine entry name for chapter `index` — the virtual path the
    /// reader's scheme handler serves the chapter's page under, and the
    /// `href` its manifest TOC rows carry. `chapter-0`, `chapter-1`, …,
    /// 0-based like a PDF's `page-N` and a deck's `slide-N`.
    pub fn spine_name(index: usize) -> String {
        format!("chapter-{index}")
    }

    /// The whole book's spine, in order.
    pub fn spine(&self) -> Vec<String> {
        (0..self.chapters.len()).map(Self::spine_name).collect()
    }

    /// Chapter `index`'s image by reference name (`000123` for an
    /// `img/000123` src), if the book carries it.
    pub fn image(&self, name: &str) -> Option<&BookImage> {
        self.images.iter().find(|image| {
            image
                .name
                .strip_prefix("img/")
                .is_some_and(|rest| rest == name)
        })
    }
}

/// Which parser a file extension goes to. One table, read by the import
/// path, the BookDrop receiver, the file chooser and the CLI — the same
/// one-list discipline `DocumentBook.supportedExtensions` keeps on iOS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocFamily {
    /// `.mobi`/`.azw`/`.azw3`/`.prc`/`.pdb` — one parser, the whole
    /// Mobipocket/Kindle family (PalmDOC and KF8 variants alike).
    Mobi,
    /// `.fb2` — FictionBook.
    Fb2,
    /// `.rtf` — Rich Text.
    Rtf,
    /// `.html`/`.htm`/`.htmlz` — web pages saved as books.
    Html,
    /// `.txt`/`.text` — plain-text books.
    Txt,
}

impl DocFamily {
    /// The family a file extension imports as, or `None` when the
    /// extension is not a headless book format this crate reads.
    pub fn from_extension(extension: &str) -> Option<Self> {
        match extension.to_ascii_lowercase().as_str() {
            "mobi" | "azw" | "azw3" | "prc" | "pdb" => Some(Self::Mobi),
            "fb2" => Some(Self::Fb2),
            "rtf" => Some(Self::Rtf),
            "html" | "htm" | "htmlz" => Some(Self::Html),
            "txt" | "text" => Some(Self::Txt),
            _ => None,
        }
    }

    /// Whether `extension` names a headless book format. The cheap half of
    /// [`Self::from_extension`], for lists that only need the yes/no.
    pub fn is_document_extension(extension: &str) -> bool {
        Self::from_extension(extension).is_some()
    }
}

/// Wrap a chapter's body XHTML into the standalone page the reader's web
/// engine loads. The reader's own script (`window.__sn`) is injected into
/// every frame by the view, so the page needs nothing but the body — no
/// styles, no scripts of its own: the appearance layer dresses it.
pub fn chapter_page(title: &str, body_html: &str) -> String {
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\">\
         <title>{}</title></head><body>{}</body></html>",
        xml_escape(title),
        body_html
    )
}

/// Escape a string for text content and double-quoted attribute values.
pub fn xml_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

/// Latin-1: every byte is a glyph, byte for byte. The floor every decoding
/// battery lands on, because it never fails.
pub fn decode_latin1(data: &[u8]) -> String {
    data.iter().map(|&b| b as char).collect()
}

/// Windows-1252: latin-1, except the 0x80–0x9F range the Windows codepage
/// maps to typographic punctuation (curly quotes, dashes, the euro sign).
/// The bytes web pages and RTF actually carry.
///
/// The tables live in [`crate::codepages`] — one set for every codepage the
/// book formats name, verified against the reference mappings rather than
/// written out by hand.
pub fn decode_cp1252(data: &[u8]) -> String {
    crate::codepages::decode_cp1252(data)
}

/// Windows-1251: the Cyrillic codepage the Russian FictionBook corpus ships
/// in. 0xC0–0xFF are the alphabet in order; the punctuation and the boxed
/// letters sit in 0x80–0xBF.
pub fn decode_cp1251(data: &[u8]) -> String {
    crate::codepages::decode_cp1251(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_land_on_their_family() {
        for ext in ["mobi", "azw", "azw3", "prc", "pdb", "MOBI", "Azw3"] {
            assert_eq!(
                DocFamily::from_extension(ext),
                Some(DocFamily::Mobi),
                "{ext} is the Mobipocket family"
            );
        }
        assert_eq!(DocFamily::from_extension("fb2"), Some(DocFamily::Fb2));
        assert_eq!(DocFamily::from_extension("RTF"), Some(DocFamily::Rtf));
        for ext in ["html", "htm", "htmlz"] {
            assert_eq!(DocFamily::from_extension(ext), Some(DocFamily::Html));
        }
        assert_eq!(DocFamily::from_extension("txt"), Some(DocFamily::Txt));
        assert_eq!(DocFamily::from_extension("text"), Some(DocFamily::Txt));
    }

    #[test]
    fn near_miss_extensions_are_not_documents() {
        // `mobile`, `mobi3`, `fbz`, `text/plain` — the wrong-form traps the
        // starts_with family of bugs produce.
        for ext in ["", "mobile", "mobi3", "mob", "azw4", "fbz", "rt", "html5", "txt2", "epub"] {
            assert_eq!(DocFamily::from_extension(ext), None, "{ext:?} is not ours");
        }
        assert!(!DocFamily::is_document_extension("epub"));
        assert!(DocFamily::is_document_extension("pdb"));
    }

    #[test]
    fn the_spine_names_chapters_in_order() {
        let doc = BookDoc {
            chapters: vec![BookChapter::default(), BookChapter::default()],
            ..BookDoc::default()
        };
        assert_eq!(doc.spine(), vec!["chapter-0", "chapter-1"]);
        assert_eq!(BookDoc::spine_name(11), "chapter-11");
    }

    #[test]
    fn images_resolve_by_their_reference_name() {
        let doc = BookDoc {
            images: vec![
                BookImage {
                    name: "img/cover".into(),
                    mime: "image/jpeg".into(),
                    data: vec![1, 2],
                },
                BookImage {
                    name: "img/000123".into(),
                    mime: "image/png".into(),
                    data: vec![3],
                },
            ],
            ..BookDoc::default()
        };
        assert!(doc.image("000123").is_some());
        assert!(doc.image("cover").is_some());
        assert!(doc.image("img/cover").is_none(), "the src, not the name");
        assert!(doc.image("missing").is_none());
    }

    #[test]
    fn a_chapter_page_is_a_standalone_document() {
        let page = chapter_page(
            "Chapter & One",
            "<h1>Hi</h1><p>Body &amp; more</p>",
        );
        assert!(page.starts_with("<!DOCTYPE html>"), "{page}");
        assert!(page.contains("<title>Chapter &amp; One</title>"), "{page}");
        assert!(page.contains("<body><h1>Hi</h1><p>Body &amp; more</p></body>"), "{page}");
        // The body is the parser's own XHTML: passed through, not re-escaped.
        assert!(!page.contains("&amp;amp;"), "{page}");
    }

    #[test]
    fn escaping_covers_the_five_markup_characters() {
        assert_eq!(xml_escape("a<b>&\"'c"), "a&lt;b&gt;&amp;&quot;&#39;c");
        assert_eq!(xml_escape(""), "");
        assert_eq!(xml_escape("plain text 123"), "plain text 123");
    }
}
