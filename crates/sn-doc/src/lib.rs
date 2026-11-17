//! `sn-doc` — the document layer: EPUB import, chapter text, and (later
//! batches) PDF extraction.
//!
//! The iOS port rendered EPUB with epub.js in a webview and kept a *native*
//! parser for everything the library shelf needs — title, author, TOC, cover,
//! reading order — because a shelf of books must cost a few XML reads, not one
//! web process per book. This crate keeps that split:
//!
//! * [`zip`] — a read-only ZIP reader, enough for EPUB containers, with the
//!   central-directory traps the family already knows about.
//! * [`xml`] — a minimal pull parser. EPUB's metadata is XML; the alternative
//!   (`quick-xml`) has a 1.86 MSRV against this workspace's 1.85, and a
//!   hundred lines of pull-parser covers exactly the subset EPUB needs.
//! * [`epub`] — container.xml → OPF → spine/TOC/cover, as `EpubBook`.
//! * [`pdf`] — the container walk: title, author, page count, outline, and
//!   encryption detection, from the trailer and the page tree alone.
//! * [`pdftext`] — page text: the papero layout over a content-stream
//!   interpreter.
//! * [`pdfium_source`] — the PDFium glyph source: Chrome's engine decodes
//!   the glyphs, papero's layout reads them. The content-stream interpreter
//!   is the fallback when no pdfium library is bound.
//! * [`mp4`] — MPEG-4 metadata, chapters and duration for audiobooks.
//! * [`docx`] — office import: one `.docx`'s WordprocessingML to note-shaped
//!   markdown, images included.
//! * [`bookdoc`] — the shared model the headless book formats reduce to
//!   (Mobipocket/Kindle, FictionBook, RTF, HTML/HTMLZ, plain text), with the
//!   extension→family table the import path, BookDrop and the CLI all read.
//!   The parsers themselves are [`mobi`], [`fb2`], [`rtf`], [`htmlbook`] and
//!   [`plainbook`]; the original file stays on disk verbatim, never converted.
//! * [`ocr`] — OCR for scanned pages: rasterise with pdftoppm, read with
//!   RapidOCR (PaddleOCR-class ONNX models) when the runtime and models are
//!   present, tesseract otherwise; degrade to empty text when neither is.
//! * [`slides`] / [`odp`] — the slide decks: pptx (through DeckCraft's
//!   clean-room ECMA-376 reader) and odp resolved into one `SlideDeck` model
//!   the deck viewer draws.

pub mod bookdoc;
pub mod chapter;
pub mod codepages;
pub mod docx;
pub mod epub;
pub mod fb2;
pub mod htmlbook;
pub mod huffcdic;
pub mod mobi;
pub mod mp4;
pub mod odp;
pub mod pdf;
pub mod pdfium_source;
pub mod pdftext;
pub mod plainbook;
pub mod rtf;
pub mod slides;
pub mod xml;
pub mod zip;

pub mod ocr;
pub mod rapidocr;
pub mod reconcile;
pub mod textquality;

#[cfg(test)]
mod slides_test;
