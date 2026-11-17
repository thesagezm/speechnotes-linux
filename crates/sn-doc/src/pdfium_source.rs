//! The PDFium glyph source: Chrome's PDF engine decodes, papero lays out.
//!
//! `pdftext`'s layout pass consumes glyph boxes; where the boxes come from
//! is a pluggable question. The hand-written content-stream interpreter (the
//! original source) decodes fonts itself and inherits every PDF font edge
//! case — CMaps, encodings, widths — as its own bugs. PDFium has already
//! seen every broken file on the internet: the same glyphs, decoded by the
//! engine the browser uses, fed into the same layout thresholds.
//!
//! The library is found, not linked: `libpdfium.so` ships in the app's tools
//! prefix the way tesseract does (no sudo, no system package), a system
//! install is honoured, and `SPEECHNOTES_PDFIUM_LIB` overrides both. When
//! the library cannot be bound or the file it opens turns out unreadable,
//! the caller falls back to the interpreter — the same extraction, one
//! engine older.
//!
//! What papero's [`Char`] needs, and where it comes from here: box —
//! `FPDFText_GetCharBox` (tight bounds), mirrored from PDF's y-up page
//! space into the layout's top-down screen space; size — the scaled font
//! size (the Tm scale every threshold divides by); space — a painted space
//! glyph sets a pending flag, exactly the interpreter's rule; run —
//! PDFium's char order is the content stream's, and a paint run is
//! approximated by identity: a change of colour, font or size, a backwards
//! pen, or a jump clear of the previous glyph's line starts a new run.
//! Runs go in x order and keep stream order within themselves, so
//! overlapping runs cannot interleave — which is the whole point of runs.
//!
//! Shadow copies (a title's outline painted under its fill, an offset
//! shadow) survive here as two whole runs; the layout's line-level
//! duplicate collapse — the interpreter's own, shared by both engines —
//! drops the copy. Splitting runs finer than that shatters a copy into
//! fragments, and x-sorting fragments is the interleaving bug all over
//! again.
//!
//! One behaviour measured on real books and left as pdfium's: a line-end
//! hyphen is swallowed ("Pos-" / "teriorly" arrives as "Pos", "teriorly" —
//! glyph gone, no space flag between), so the layout cannot re-join the
//! word. The content-stream interpreter sees the hyphen glyph and papero's
//! hyphen-undo re-joins it; on prose-heavy books that makes the
//! interpreter the better reading engine even where pdfium decodes every
//! glyph. When a box binds pdfium, the A/B is
//! [`crate::pdftext::extract_chars`] (pdfium) versus
//! [`crate::pdftext::extract_chars_interpreted`].

// Three corrupt fragments are repaired here (NUL-run misspellings of
// "honoured", "load_pdf_from_bytes_slice", and the "0.05 * size.max(prev.size)"
// spacing) — each repaired spelling confirmed against real APIs / phrases.

use std::path::PathBuf;
use std::sync::OnceLock;

use pdfium_render::prelude::*;

use crate::ocr::tools_root;
use crate::pdftext::Char;

/// The bound pdfium, resolved once per process and leaked deliberately: a
/// dynamic library cannot be meaningfully unloaded while sessions may still
/// borrow its state, and one per process is the engine's own model. `None`
/// means "not available" — every caller falls back to the interpreter.
static LIBRARY: OnceLock<Option<&'static Pdfium>> = OnceLock::new();

/// The bound pdfium, or `None` when no library could be bound.
pub fn library() -> Option<&'static Pdfium> {
    *LIBRARY.get_or_init(bind)
}

fn bind() -> Option<&'static Pdfium> {
    if let Some(path) = library_path() {
        match Pdfium::bind_to_library(&path) {
            Ok(bindings) => {
                log::info!("pdfium glyph source bound: {}", path.display());
                return Some(Box::leak(Box::new(Pdfium::new(bindings))));
            }
            Err(e) => log::warn!("pdfium at {} unusable ({e}); trying the system", path.display()),
        }
    }
    match Pdfium::bind_to_system_library() {
        Ok(bindings) => {
            log::info!("pdfium glyph source bound: system loader");
            Some(Box::leak(Box::new(Pdfium::new(bindings))))
        }
        Err(e) => {
            log::info!("no pdfium library ({e}); text falls back to the content-stream interpreter");
            None
        }
    }
}

/// Where to look for `libpdfium.so`, in order: an explicit override, the
/// package layouts (system install, user install), the app's own tools
/// prefix (what install.sh and the no-sudo installs use), and — absent all
/// — the system loader via `bind_to_system_library`.
fn library_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("SPEECHNOTES_PDFIUM_LIB") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
        log::warn!("SPEECHNOTES_PDFIUM_LIB points at nothing: {}", path.display());
    }
    let mut candidates = vec![PathBuf::from("/usr/lib/speechnotes/libpdfium.so")];
    if let Some(appdir) = std::env::var_os("APPDIR") {
        let mut p = PathBuf::from(appdir);
        p.push("usr/lib/speechnotes/libpdfium.so");
        candidates.push(p);
    }
    if let Some(home) = std::env::var_os("HOME") {
        let mut home = PathBuf::from(home);
        home.push(".local/lib/speechnotes/libpdfium.so");
        candidates.push(home);
    }
    candidates.push(tools_root().join("lib/libpdfium.so"));
    candidates.into_iter().find(|p| p.is_file())
}

/// A store document opened in pdfium. The bytes are shared, not copied:
/// pdfium reads the same buffer the interpreter would.
pub struct PdfiumTextSource<'a> {
    doc: PdfDocument<'a>,
}

impl<'a> PdfiumTextSource<'a> {
    /// Open the parsed document in pdfium. `None` when no pdfium library is
    /// bound or the file defeats it — the caller falls back to the
    /// content-stream interpreter, so a malformed file costs nothing.
    pub fn open(doc: &'a crate::pdf::Document) -> Option<Self> {
        let lib = library()?;
        match lib.load_pdf_from_byte_slice(doc.raw_bytes(), None) {
            Ok(doc) => Some(Self { doc }),
            Err(e) => {
                log::debug!("pdfium could not open the file: {e}");
                None
            }
        }
    }

    /// One page's painted glyphs in papero's shape. `page` is 0-based. A
    /// page pdfium cannot read comes back empty — for the store that is the
    /// OCR path's cue, the same as a page with no text layer.
    pub fn page_chars(&self, page: usize) -> Vec<Char> {
        let Ok(page) = self.doc.pages().get(page as i32) else {
            return Vec::new();
        };
        // PdfRect hides its edges; the quad points state them. `top` is the
        // page's y-up ceiling, the pivot of the y mirror below.
        let top = f64::from(page.page_size().to_quad_points().top().value);
        let Ok(text) = page.text() else {
            return Vec::new();
        };

        let mut out: Vec<Char> = Vec::new();
        let mut run: u32 = 0;
        let mut pending_space = false;
        let mut identity: Option<(u8, u8, u8, u8, String, f64)> = None;

        for ch in text.chars().iter() {
            let Some(c) = ch.unicode_char() else { continue };
            match c {
                // A painted space is a statement about the next glyph, not a
                // glyph of its own — the interpreter's rule, honoured even
                // when the boxes overlap.
                ' ' | '\t' | '\u{a0}' => {
                    pending_space = true;
                    continue;
                }
                '\r' | '\n' => continue,
                c if (c as u32) < 32 => continue,
                _ => {}
            }
            // The LOOSE box for x — metric-true widths. For y, papero wants
            // what the interpreter derives from the pen: a uniform band per
            // line, anchored on the baseline. PDFium's loose box is not
            // quite uniform (an `i`'s top over an `a`'s), and its internal
            // char order sorts on that difference — a y-then-x order that
            // must never survive as stream order.
            let Ok(rect) = ch
                .loose_bounds()
                .or_else(|_| ch.tight_bounds())
                .map(|r| r.to_quad_points())
            else {
                continue;
            };
            let Ok((_, baseline_up)) = ch.origin() else {
                continue;
            };
            let baseline_up = f64::from(baseline_up.value);
            let size = f64::from(ch.scaled_font_size().value);
            let x0 = f64::from(rect.left().value);
            let x1 = f64::from(rect.right().value);
            // PDF's y grows up; the layout reads top-down. Mirrored at push
            // time, the interpreter's way. The band: 0.75 em above the
            // baseline, 0.25 below — one line, one band, every glyph.
            let y0 = top - baseline_up - 0.75 * size;
            let y1 = top - baseline_up + 0.25 * size;

            let next = match (ch.fill_color(), ch.font_name()) {
                (Ok(color), font) => Some((color.red(), color.green(), color.blue(), color.alpha(), font, size)),
                _ => None,
            };
            // Run boundaries, and why. PDFium's char order is y-then-x, so
            // within a line the pen visibly jumps backwards whenever the
            // next glyph's band edge differs — that jump must split a run,
            // so the layout's x-order of runs re-sorts the line into
            // reading order. Overlapping copies (a title's shadow painted
            // after its fill) must survive as two whole runs for the
            // layout's duplicate collapse, which a kerning-sized backward
            // move never splits: only a real jump (5% of an em) does.
            let new_run = match (identity.as_ref(), out.last()) {
                (Some(prev_id), Some(prev)) => {
                    next.as_ref() != Some(prev_id)
                        || x0 < prev.x1 - 0.05 * size.max(prev.size)
                        || ((y0 + y1) / 2.0 - prev.cy()).abs() > 0.6 * size.max(prev.size)
                }
                _ => false,
            };
            if new_run {
                run += 1;
            }
            identity = next;

            out.push(Char {
                c,
                x0,
                y0,
                x1,
                y1,
                size,
                space: std::mem::take(&mut pending_space),
                run,
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal but structurally complete one-page PDF: Helvetica, one
    /// paint run, two words. Real enough for a real engine to read it.
    fn fixture() -> Vec<u8> {
        let content = b"BT /F1 24 Tf 72 720 Td (Hello pdfium) Tj ET";
        let mut objects: Vec<String> = Vec::new();
        objects.push("1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".into());
        objects.push("2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".into());
        objects.push(
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>\nendobj\n"
                .into(),
        );
        objects.push(
            "4 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n".into(),
        );
        objects.push(format!(
            "5 0 obj\n<< /Length {} >>\nstream\n{}endstream\nendobj\n",
            content.len(),
            String::from_utf8_lossy(content)
        ));
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for object in &objects {
            offsets.push(pdf.len() as u32);
            pdf.extend_from_slice(object.as_bytes());
        }
        let xref_at = pdf.len();
        pdf.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
        pdf.extend_from_slice(b"0000000000 65535 f \n");
        for offset in &offsets {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
                objects.len() + 1,
                xref_at
            )
            .as_bytes(),
        );
        pdf
    }

    #[test]
    fn pdfium_reads_a_plain_fixture_when_bound() {
        let bytes = fixture();
        let Ok(doc) = crate::pdf::Document::parse(&bytes) else {
            // The hand-built fixture must parse with the store's own parser
            // for the comparison to mean anything.
            panic!("fixture does not parse");
        };
        let Some(src) = PdfiumTextSource::open(&doc) else {
            // No pdfium on this box (or the fixture defeats the engine):
            // the fallback world, where there is nothing to assert.
            eprintln!("pdfium not available — test passes vacuously");
            return;
        };
        let chars = src.page_chars(0);
        let text: String = chars.iter().map(|c| c.c).collect();
        assert_eq!(text, "Hellopdfium");
        // One paint run, one word gap: the painted space rides as a flag on
        // the next glyph, not as a glyph of its own.
        assert!(chars.iter().all(|c| c.run == 0), "fixture is one run");
        let p = chars.iter().find(|c| c.c == 'p').expect("p in output");
        assert!(p.space, "pdfium's painted space flags the next glyph");
        // y mirrored: the line painted at 720 (y-up) sits near the top of a
        // 792-point page, well under halfway down.
        assert!(chars[0].y0 < 792.0 / 2.0, "y must be mirrored top-down");
    }
}
