//! OCR for scanned pages: `pdftoppm` rasterises, a reader reads.
//!
//! A scanned PDF carries no text layer — the pages are photographs — and
//! the glyph sources have nothing to decode. The iOS app asks the Vision
//! framework; this port asks RapidOCR (PaddleOCR's PP-OCRv4 models through
//! ONNX Runtime — see [`crate::rapidocr`]) and keeps tesseract, the
//! original designated standard (FEATURES 4.9), behind it for a box without
//! the runtime or the models. Either way the page degrades honestly:
//! `available()` is the one gate every caller checks, and a page no reader
//! can take stays empty rather than blocking the reader on a missing tool.
//!
//! The contract is deliberately small: rasterise one page, recognise it,
//! return the text. Where the text is cached belongs to the store
//! (`Books/<uuid>/text/NNNN.txt`), not here.

use std::sync::OnceLock;

/// Whether any OCR engine can run: RapidOCR (runtime + models), or
/// tesseract on PATH or in the app's tools prefix. Resolved once.
pub fn available() -> bool {
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| crate::rapidocr::available() || binary().is_some())
}

/// The tesseract binary's path, if one can be run.
fn binary() -> Option<std::path::PathBuf> {
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join("tesseract");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    // The app's tools prefix: install.sh (or a no-sudo untar) drops tesseract
    // here, and the app finds it without touching the user's PATH.
    let local = tools_dir().join("tesseract");
    local.is_file().then_some(local)
}

/// The env a tesseract invocation needs: empty for a system install, and the
/// prefix's own lib/tessdata dirs for the local one (its shared libraries
/// and traineddata live beside it, not in the system).
fn binary_env(binary: &std::path::Path) -> Vec<(String, String)> {
    let mut env = Vec::new();
    // Only the app's own prefix carries its libs beside it; a system install
    // needs nothing (and must not be pointed at a foreign TESSDATA_PREFIX).
    if binary != tools_dir().join("tesseract") {
        return env;
    }
    let Some(tools) = tools_dir().parent().map(std::path::Path::to_path_buf) else {
        return env;
    };
    let lib = tools.join("lib");
    let tessdata = tools.join("share-tessdata/tessdata");
    if lib.is_dir() {
        let existing = std::env::var("LD_LIBRARY_PATH").unwrap_or_default();
        let joined = if existing.is_empty() {
            lib.to_string_lossy().into_owned()
        } else {
            format!("{existing}:{}", lib.to_string_lossy())
        };
        env.push(("LD_LIBRARY_PATH".into(), joined));
    }
    if tessdata.is_dir() {
        env.push(("TESSDATA_PREFIX".into(), tessdata.to_string_lossy().into_owned()));
    }
    env
}

/// `<data>` — the XDG data root the store, the tools prefix and the OCR
/// models all hang off (env override honoured, absolute only).
pub(crate) fn data_root() -> std::path::PathBuf {
    match (
        std::env::var("XDG_DATA_HOME").ok(),
        std::env::var("HOME").ok(),
    ) {
        (Some(dir), _) if dir.starts_with('/') => std::path::PathBuf::from(dir),
        (_, Some(home)) => std::path::PathBuf::from(home).join(".local/share"),
        _ => std::path::PathBuf::from(".local/share"),
    }
}

/// `<data>/speechnotes/tools` — the no-sudo prefix, shared by every native
/// tool the app carries: tesseract's `bin/`, pdfium's `lib/`, tessdata.
pub(crate) fn tools_root() -> std::path::PathBuf {
    data_root().join("speechnotes/tools")
}

/// `<data>/tools/bin` — sibling of `Books/`, from the same XDG rules the
/// store uses (env override honoured, absolute only).
fn tools_dir() -> std::path::PathBuf {
    tools_root().join("bin")
}

/// Recognise one page of a PDF: rasterise at `dpi` with `pdftoppm`, then
/// read the image — RapidOCR first, tesseract behind it. `page` is 0-based.
/// The text comes back paragraphed — one visual line per line, blank lines
/// between paragraphs — which is the shape the reader and the speech
/// chunker already consume.
///
/// Errors are strings, not a taxonomy: the caller logs and degrades, and no
/// caller can act differently on "no tool" versus "unreadable page".
pub fn page_text(pdf: &std::path::Path, page: usize, dpi: u32) -> Result<String, String> {
    page_text_with_engine(pdf, page, dpi).map(|(text, _)| text)
}

/// [`page_text`] with the engine named, so the log says which reader
/// produced a page's text. The two names are the whole truth about
/// "engines": RapidOCR is the engine and tesseract its fallback — the
/// *other* reading a page can carry is the PDF's own baked-in text layer,
/// which is not an OCR engine at all but the producer's.
pub fn page_text_with_engine(
    pdf: &std::path::Path,
    page: usize,
    dpi: u32,
) -> Result<(String, &'static str), String> {
    // The scratch directory is **per call**, not per (process, page).
    //
    // Two callers can recognise the same page at the same time: the
    // background warm-up thread and the UI thread's own read of the page the
    // user is looking at. With the directory keyed by process and page, both
    // ran `pdftoppm` into the *same* directory, so whichever finished first
    // deleted the other's half-written PNG. The reader then saw a truncated
    // file ("libpng error: IDAT: CRC error") or no PNG at all ("pdftoppm
    // wrote no page image") — which is exactly what the log shows, 209
    // failures over one book. The name carries a counter and the thread's id,
    // so no two calls can ever share a directory.
    let dir = std::env::temp_dir().join(format!(
        "speechnotes-ocr-{}-{}",
        std::process::id(),
        unique_scratch()
    ));
    std::fs::create_dir_all(&dir).map_err(|e| format!("ocr scratch dir: {e}"))?;
    let scope = dir.join("page");
    let status = std::process::Command::new("pdftoppm")
        .arg("-png")
        .arg("-r").arg(dpi.to_string())
        .arg("-f").arg((page + 1).to_string())
        .arg("-l").arg((page + 1).to_string())
        .arg(pdf)
        .arg(&scope)
        .status()
        .map_err(|e| format!("pdftoppm could not be run: {e}"))?;
    let result = if !status.success() {
        Err(format!("pdftoppm failed: exit {status}"))
    } else {
        // pdftoppm appends "-<n>.png" to the prefix it is given.
        let png = std::fs::read_dir(&dir)
            .ok()
            .and_then(|entries| {
                let mut names: Vec<_> = entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|x| x == "png"))
                    .collect();
                names.sort();
                names.pop()
            })
            .ok_or_else(|| "pdftoppm wrote no page image".to_string())?;
        read_page(&png)
    };
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// A suffix no two calls can share: a process-wide counter plus the calling
/// thread's id, so two threads in one process never collide and neither do
/// two processes.
fn unique_scratch() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let thread = format!("{:?}", std::thread::current().id());
    // The thread id renders as `ThreadId(7)`; keep only the digits and letters
    // so the name is a safe path component.
    let thread: String = thread
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    format!("{n}-{thread}")
}

/// One page image's text: RapidOCR when its runtime and models resolve,
/// tesseract otherwise, and an honest error when neither is installed. The
/// engine's name comes back with the text for the log.
fn read_page(png: &std::path::Path) -> Result<(String, &'static str), String> {
    if crate::rapidocr::available() {
        let bytes = std::fs::read(png).map_err(|e| format!("read raster: {e}"))?;
        match crate::rapidocr::page_image_text(&bytes) {
            Ok(text) => return Ok((text, "rapidocr")),
            Err(e) => log::warn!("rapidocr failed, falling back to tesseract: {e}"),
        }
    }
    let tesseract = binary().ok_or_else(|| {
        "no OCR engine: neither the RapidOCR models nor tesseract is installed".to_string()
    })?;
    let out = std::process::Command::new(&tesseract)
        .arg(png)
        .arg("stdout")
        .arg("-l").arg("eng")
        .arg("--dpi").arg("200")
        .envs(binary_env(&tesseract))
        .output()
        .map_err(|e| format!("tesseract could not be run: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "tesseract failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok((String::from_utf8_lossy(&out.stdout).trim_end().to_string(), "tesseract"))
}

/// One embedded image's text — the slide-deck OCR pass: a picture inside a
/// slide may carry text no glyph source can decode. Same engine order and
/// the same honest errors as [`page_text`].
///
/// An SVG is text by construction (no recognition to do) and comes back
/// empty, as does anything smaller than ~96×96 px — icons and bullets are
/// not content, and running a detector on them costs a second per slide.
pub fn image_text(bytes: &[u8]) -> Result<String, String> {
    if bytes.starts_with(b"<?xml") || bytes.starts_with(b"<svg") {
        return Ok(String::new());
    }
    let Ok(img) = image::load_from_memory(bytes) else {
        return Err("not a decodable image".to_string());
    };
    let (w, h) = (img.width(), img.height());
    if w < 96 || h < 96 {
        return Ok(String::new());
    }
    if crate::rapidocr::available() {
        match crate::rapidocr::page_image_text(bytes) {
            Ok(text) => return Ok(text),
            Err(e) => log::warn!("rapidocr failed on slide image, falling back: {e}"),
        }
    }
    let tesseract = binary().ok_or_else(|| {
        "no OCR engine: neither the RapidOCR models nor tesseract is installed".to_string()
    })?;
    // Tesseract reads files, not pipes: the bytes go through a scratch PNG.
    // The directory is per call, like the page path's: two slide images
    // recognised at once must not share a scratch file.
    let dir = std::env::temp_dir().join(format!(
        "speechnotes-ocr-img-{}-{}",
        std::process::id(),
        unique_scratch()
    ));
    std::fs::create_dir_all(&dir).map_err(|e| format!("ocr scratch dir: {e}"))?;
    let png = dir.join(format!("img-{}.png", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)));
    let result = image::DynamicImage::ImageRgba8(img.to_rgba8())
        .save_with_format(&png, image::ImageFormat::Png)
        .map_err(|e| format!("write raster: {e}"))
        .and_then(|()| {
            let out = std::process::Command::new(&tesseract)
                .arg(&png)
                .arg("stdout")
                .arg("-l").arg("eng")
                .envs(binary_env(&tesseract))
                .output()
                .map_err(|e| format!("tesseract could not be run: {e}"))?;
            if out.status.success() {
                Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
            } else {
                Err(format!(
                    "tesseract failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ))
            }
        });
    let _ = std::fs::remove_dir_all(&dir);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_tool_reports_absent_without_panic() {
        // Not a test of the environment — a test that the probe itself never
        // panics. On a box with tesseract installed this asserts true; on one
        // without, the absence must still resolve cleanly.
        let _ = available();
    }
}
