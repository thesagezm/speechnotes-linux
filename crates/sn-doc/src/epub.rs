//! EPUB metadata: `META-INF/container.xml` → the OPF package → spine, TOC
//! and cover, without rendering anything.
//!
//! This is the iOS port's `EpubParser` in Rust, contracts carried over
//! exactly: the library shelf must never open a webview, and a shelf of
//! books must cost a handful of small XML reads. What it produces:
//!
//! * `spine` — entry paths in reading order (EPUB2 `<spine>` +
//!   `<itemref idref>`; EPUB3 defaults to the manifest's document items when
//!   there is no spine).
//! * `toc` — the nav document (EPUB3 `<nav epub:type="toc">`) or the NCX
//!   (EPUB2 `<navPoint>`), each entry's `href` resolved against the OPF's
//!   own directory with the fragment stripped, plus nesting depth.
//! * `cover` — EPUB3 `properties="cover-image"` or EPUB2
//!   `<meta name="cover">`.
//! * metadata — title, creator, language, identifier.
//!
//! The traps the family already knows, all re-checked in tests below:
//! hrefs are resolved relative to the *OPF's directory*, not the archive
//! root; spine items may live anywhere in the zip; the TOC's own hrefs
//! resolve against the *nav document's* directory, which is usually the
//! OPF's but is not required to be.

use crate::xml::{Attr, Event, Parser as XmlParser};
// Adapted to the struct-variant `Event` API of `crate::xml` (an older copy of
// this file predates the parser refactor and still spoke the older
// tuple-event + thread-local-name contract). Only the event patterns and the
// now-obsolete name helpers changed; every parse decision is unchanged.

use crate::zip::Archive;

/// One table-of-contents row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TocEntry {
    pub label: String,
    /// Zip entry path, fragment stripped.
    pub href: String,
    /// The `#anchor` the row's href carried, if any — what the reader scrolls
    /// to inside the chapter.
    //  field authored from the call site (sn-store books.rs
    // `ManifestTocEntry { fragment: t.fragment.clone(), .. }`, an
    // `Option<String>`); no original of this field surfaced. Populated from the
    // href's `#` suffix.
    pub fragment: Option<String>,
    /// Index into `spine` of the chapter this row points at, if it matches
    /// one — what the reader jumps to and what per-chapter bookmarks use.
    pub spine_index: Option<usize>,
    /// Nesting depth from the TOC document (0 = top).
    pub depth: usize,
}

/// What the shelf and reader need without rendering.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EpubInfo {
    pub title: Option<String>,
    pub creator: Option<String>,
    pub language: Option<String>,
    pub identifier: Option<String>,
    /// Zip entry paths of the spine items, in reading order.
    pub spine: Vec<String>,
    pub toc: Vec<TocEntry>,
    /// Zip entry path of the cover image, if found.
    pub cover: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum EpubError {
    #[error("no META-INF/container.xml, or it names no rootfile")]
    MissingContainer,
    #[error("the OPF {path:?} is missing from the archive")]
    MissingOpf { path: String },
    #[error("the OPF could not be read: {0}")]
    MalformedOpf(String),
    #[error("zip: {0}")]
    Zip(#[from] crate::zip::ZipError),
}

/// Parse an EPUB archive's metadata.
pub fn parse(archive: &Archive) -> Result<EpubInfo, EpubError> {
    // 1. container.xml → rootfile path (relative to the archive root).
    let container = archive
        .read_string("META-INF/container.xml")
        .map_err(|_| EpubError::MissingContainer)?;
    let opf_path = rootfile_path(&container).ok_or(EpubError::MissingContainer)?;
    let opf = archive
        .read_string(&opf_path)
        .map_err(|_| EpubError::MissingOpf { path: opf_path.clone() })?;

    // 2. OPF: metadata, manifest, spine.
    let opf_dir = dir_of(&opf_path);
    let mut info = EpubInfo::default();
    let mut manifest: Vec<(String, String, String)> = Vec::new(); // (id, href, properties)
    let mut spine_ids: Vec<String> = Vec::new();
    let mut cover_meta: Option<String> = None;
    let mut nav_href: Option<String> = None;
    let mut ncx_href: Option<String> = None;

    let mut p = XmlParser::new(&opf);
    // The metadata block is the only place `creator`/`title` mean what we
    // want; track whether we are inside it.
    let mut in_metadata = false;
    let mut current: Option<String> = None; // the metadata element being read
    while let Some(event) = p.next_event().map_err(|e| EpubError::MalformedOpf(e.to_string()))? {
        match event {
            Event::Start { name, attrs } | Event::Empty { name, attrs } => {
                match local_name(&name).as_str() {
                    "metadata" => in_metadata = true,
                    "title" if in_metadata => current = Some("title".into()),
                    "creator" if in_metadata => current = Some("creator".into()),
                    "language" if in_metadata => current = Some("language".into()),
                    "identifier" if in_metadata => current = Some("identifier".into()),
                    "item" => {
                        let id = Attr::get(&attrs, "id").unwrap_or("").to_string();
                        let href = Attr::get(&attrs, "href").unwrap_or("").to_string();
                        let properties =
                            Attr::get(&attrs, "properties").unwrap_or("").to_string();
                        if properties.split_whitespace().any(|p| p == "nav") {
                            nav_href = Some(href.clone());
                        }
                        if properties.split_whitespace().any(|p| p == "cover-image") {
                            info.cover = Some(resolve(&opf_dir, &href));
                        }
                        manifest.push((id, href, properties));
                    }
                    "itemref" => {
                        let idref = Attr::get(&attrs, "idref").unwrap_or("").to_string();
                        if !idref.is_empty() {
                            spine_ids.push(idref);
                        }
                    }
                    "meta" => {
                        let name_attr = Attr::get(&attrs, "name").unwrap_or("");
                        let content = Attr::get(&attrs, "content").unwrap_or("");
                        if name_attr == "cover" && !content.is_empty() {
                            cover_meta = Some(content.to_string());
                        }
                    }
                    _ => {}
                }
            }
            Event::End { name } => {
                current.take();
                // Metadata text was captured by the Text event; the End
                // just closes the field. `</metadata>` closes the block.
                if local_name(&name) == "metadata" {
                    in_metadata = false;
                }
            }
            Event::Text(text) => {
                if let Some(field) = current.as_deref() {
                    let text = text.trim();
                    if !text.is_empty() {
                        match field {
                            "title" => info.title = Some(text.to_string()),
                            "creator" => info.creator = Some(text.to_string()),
                            "language" => info.language = Some(text.to_string()),
                            "identifier" => info.identifier = Some(text.to_string()),
                            _ => {}
                        }
                    }
                }
            }
            Event::Raw { .. } => {}
        }
    }

    // 3. Spine ids → manifest hrefs.
    let href_of = |id: &str| -> Option<(String, String)> {
        manifest
            .iter()
            .find(|(mid, _, _)| mid == id)
            .map(|(_, href, properties)| (href.clone(), properties.clone()))
    };
    let mut spine: Vec<String> = Vec::new();
    for id in &spine_ids {
        if let Some((href, _)) = href_of(id) {
            //  manifest hrefs are URI-encoded; the zip's entry names
            // are not (the original own percent_decode test pins this).
            spine.push(resolve(&opf_dir, &percent_decode(&href)));
        }
    }
    // An EPUB3 without a spine: every document item, in manifest order.
    if spine.is_empty() {
        for (_, href, properties) in &manifest {
            let is_doc = properties.is_empty()
                || properties.split_whitespace().any(|p| p == "svg");
            if is_doc && (href.ends_with(".xhtml") || href.ends_with(".html") || href.ends_with(".htm")) {
                spine.push(resolve(&opf_dir, href));
            }
        }
    }
    // The NCX id from the spine's `toc` attribute, if any.
    let ncx_from_spine = {
        // Re-read just the spine tag; a second pass is cheaper than the
        // state the first pass would have to carry.
        let mut p = XmlParser::new(&opf);
        let mut found = None;
        while let Ok(Some(event)) = p.next_event() {
            if let Event::Start { name, attrs } = &event {
                if local_name(name) == "spine" {
                    found = Attr::get(attrs, "toc").map(str::to_string);
                }
            }
        }
        found
    };
    if let Some(id) = ncx_from_spine {
        if let Some((href, _)) = href_of(&id) {
            ncx_href = Some(href);
        }
    }
    // The EPUB2 cover meta points at a manifest item by id.
    if info.cover.is_none() {
        if let Some(id) = cover_meta {
            if let Some((href, _)) = href_of(&id) {
                info.cover = Some(resolve(&opf_dir, &href));
            }
        }
    }

    info.spine = spine;

    // 4. TOC: EPUB3 nav document first, EPUB2 NCX second.
    if let Some(href) = nav_href {
        let nav_path = resolve(&opf_dir, &href);
        if let Ok(nav) = archive.read_string(&nav_path) {
            info.toc = parse_nav(&nav, dir_of(&nav_path), &info.spine);
        }
    }
    if info.toc.is_empty() {
        if let Some(href) = ncx_href {
            let ncx_path = resolve(&opf_dir, &href);
            if let Ok(ncx) = archive.read_string(&ncx_path) {
                info.toc = parse_ncx(&ncx, dir_of(&ncx_path), &info.spine);
            }
        }
    }
    // A TOC that points nowhere in the spine is still a TOC; the reader
    // maps it by path. `spine_index` stays None for those rows.

    Ok(info)
}

/// `META-INF/container.xml`'s `rootfile` full-path.
fn rootfile_path(container: &str) -> Option<String> {
    let mut p = XmlParser::new(container);
    while let Ok(Some(event)) = p.next_event() {
        //  `<rootfile/>` is an Empty event too (the original tuple-era
        // `is_start` covered both).
        if let Event::Start { name, attrs } | Event::Empty { name, attrs } = &event {
            if local_name(name) == "rootfile" {
                let path = Attr::get(attrs, "full-path")?;
                if !path.is_empty() {
                    return Some(normalize_path(path));
                }
            }
        }
    }
    None
}

/// Parse an EPUB3 nav document: the `<nav epub:type="toc">` block's anchors,
/// nesting from the `<ol>` depth.
fn parse_nav(nav: &str, nav_dir: String, spine: &[String]) -> Vec<TocEntry> {
    let mut entries = Vec::new();
    let mut p = XmlParser::new(nav);
    let mut in_toc_nav = false;
    let mut depth: usize = 0;
    let mut current_label = String::new();
    let mut pending_href: Option<String> = None;
    while let Ok(Some(event)) = p.next_event() {
        match event {
            Event::Start { name, attrs } => {
                let local = local_name(&name);
                match local.as_str() {
                    "nav" => {
                        let is_toc = Attr::get(&attrs, "epub:type")
                            .map(|t| t.split_whitespace().any(|w| w == "toc"))
                            .unwrap_or(false)
                            // EPUB 3.3 moved to `type=` without the prefix.
                            || Attr::get(&attrs, "type")
                                .map(|t| t.split_whitespace().any(|w| w == "toc"))
                                .unwrap_or(false);
                        if is_toc {
                            in_toc_nav = true;
                            depth = 0;
                        }
                    }
                    "ol" if in_toc_nav => depth += 1,
                    "a" if in_toc_nav => {
                        pending_href = Attr::get(&attrs, "href").map(str::to_string);
                        current_label.clear();
                    }
                    _ => {}
                }
            }
            Event::Empty { .. } => {}
            Event::Text(text) if pending_href.is_some() => {
                current_label.push_str(&text);
            }
            Event::End { name } => {
                match local_name(&name).as_str() {
                    "ol" if in_toc_nav => depth = depth.saturating_sub(1),
                    "nav" if in_toc_nav => {
                        //  the original `in_toc_nav = false;` here was a
                        // dead store (the loop breaks immediately) — dropped
                        // 2026-10-30 to clear the rustc warning, no behavior
                        // change.
                        break;
                    }
                    "a" => {
                        if let Some(href) = pending_href.take() {
                            let path = resolve(&nav_dir, strip_fragment(&percent_decode(&href)));
                            //  a nav href that resolves nowhere in the
                            // spine maps by file name — the spine path is the
                            // one the reader opens (the original tests pin
                            // both the resolved href and the spine index).
                            let path = spine
                                .iter()
                                .find(|s| *s == &path)
                                .or_else(|| {
                                    let base = path.rsplit('/').next()?;
                                    spine.iter().find(|s| s.rsplit('/').next() == Some(base))
                                })
                                .cloned()
                                .unwrap_or(path);
                            entries.push(TocEntry {
                                label: current_label.trim().to_string(),
                                spine_index: spine.iter().position(|s| *s == path),
                                href: path,
                                depth: depth.saturating_sub(1),
                                fragment: fragment_of(&percent_decode(&href)),
                            });
                        }
                        current_label.clear();
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    entries
}

/// Parse an EPUB2 NCX: `<navPoint>` nesting via `<navPoint>` open/close.
fn parse_ncx(ncx: &str, ncx_dir: String, spine: &[String]) -> Vec<TocEntry> {
    let mut entries = Vec::new();
    let mut p = XmlParser::new(ncx);
    let mut depth: usize = 0;
    let mut label = String::new();
    let mut pending_href: Option<String> = None;
    let mut in_navmap = false;
    let mut collecting_label = false;
    while let Ok(Some(event)) = p.next_event() {
        match event {
            //  `<content/>` is a self-closing element — Empty events
            // carry the same meaning here as Start.
            Event::Start { name, attrs } | Event::Empty { name, attrs } => {
                let local = local_name(&name);
                match local.as_str() {
                    "navMap" => in_navmap = true,
                    "navPoint" if in_navmap => {
                        depth += 1;
                        label.clear();
                        pending_href = None;
                    }
                    "text" if depth > 0 => collecting_label = true,
                    "content" => {
                        if let Some(href) = Attr::get(&attrs, "src") {
                            pending_href = Some(href.to_string());
                        }
                    }
                    _ => {}
                }
            }
            Event::Text(text) if collecting_label => label.push_str(&text),
            Event::End { name } => {
                let local = local_name(&name);
                match local.as_str() {
                    "text" => collecting_label = false,
                    "navPoint" if in_navmap => {
                        depth = depth.saturating_sub(1);
                        if let Some(href) = pending_href.take() {
                            let path =
                                resolve(&ncx_dir, strip_fragment(&percent_decode(&href)));
                            let path = spine
                                .iter()
                                .find(|s| *s == &path)
                                .or_else(|| {
                                    let base = path.rsplit('/').next()?;
                                    spine.iter().find(|s| s.rsplit('/').next() == Some(base))
                                })
                                .cloned()
                                .unwrap_or(path);
                            entries.push(TocEntry {
                                label: label.trim().to_string(),
                                spine_index: spine.iter().position(|s| *s == path),
                                href: path,
                                depth: depth,
                                fragment: fragment_of(&percent_decode(&href)),
                            });
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    entries
}

// --- small path helpers ---------------------------------------------------

/// The directory part of a zip entry path (`OEBPS/foo.opf` → `OEBPS`).
fn dir_of(path: &str) -> String {
    match path.rfind('/') {
        Some(i) => path[..i].to_string(),
        None => String::new(),
    }
}

/// The `#fragment` suffix of an href, `None` when it carries none.
//  helper for the `TocEntry::fragment` field (see the struct).
fn fragment_of(href: &str) -> Option<String> {
    href.split_once('#').map(|(_, f)| f.to_string())
}

/// The content type for a zip entry name, by extension.
//  authored from the call site (sn-ui epubview.rs serves each
// archive entry to the webview as
// `sn_doc::epub::mime_for(&name)`); the original epub.rs has no mime helper
// and the final epub.rs region carrying it did not survive. The table
// covers the entry types an EPUB legitimately carries.
pub fn mime_for(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("");
    match ext.to_ascii_lowercase().as_str() {
        "xhtml" | "html" | "htm" => "application/xhtml+xml",
        "css" => "text/css",
        "js" => "text/javascript",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ncx" => "application/x-dtbncx+xml",
        "opf" => "application/oebps-package+xml",
        "txt" => "text/plain",
        "xml" => "application/xml",
        _ => "application/octet-stream",
    }
}

/// Strip a `#fragment` suffix.
fn strip_fragment(href: &str) -> &str {
    match href.find('#') {
        Some(i) => &href[..i],
        None => href,
    }
}

/// Resolve `href` against a base directory, both zip paths. Handles `../`
/// segments, the form Gutenberg books use.
fn resolve(base_dir: &str, href: &str) -> String {
    if href.is_empty() {
        return String::new();
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

/// A path with `./` segments removed and any leading `/` dropped.
fn normalize_path(path: &str) -> String {
    resolve("", path.trim_start_matches('/'))
}

/// Percent-decode a URI component (`%20` → space). EPUB hrefs are
/// URI-encoded; the zip's entry names are not.
//  `pub(crate)` — chapter.rs (its own original) calls
// `crate::epub::percent_decode`; the original epub kept it private.
// Security-review fix (2026-10-30): the original sliced `&s[i+1..i+3]`
// for the hex digits, which panics when a multibyte UTF-8 char straddles index
// i+3 (e.g. an href "%Aé" — the é's bytes sit at 2..4, so index 3 is inside
// it). Reachable from any manifest/nav/chapter href and any webview-requested
// scheme URI — untrusted EPUB bytes could crash the reader. Now the two bytes
// after '%' are checked as ASCII hex digits directly; well-formed input
// decodes identically.
pub fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    fn hex_val(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(hi << 4 | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The bare local name of a possibly-prefixed element (`dc:title` → `title`).
fn local_name(name: &str) -> String {
    match name.rfind(':') {
        Some(i) => name[i + 1..].to_string(),
        None => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml::decode_entities;
    use crate::zip::Archive;

    fn archive_with(files: &[(&str, &str)]) -> Archive {
        // Build a stored zip by hand (the same helper shape as zip.rs's
        // tests, but storing UTF-8 payloads).
        let mut data: Vec<u8> = Vec::new();
        let mut central: Vec<u8> = Vec::new();
        for (name, body) in files {
            let payload = body.as_bytes();
            let crc = crate::zip::crc32(payload);
            let offset = data.len() as u32;
            data.extend_from_slice(b"PK\x03\x04");
            data.extend_from_slice(&20u16.to_le_bytes());
            data.extend_from_slice(&0u16.to_le_bytes());
            data.extend_from_slice(&0u16.to_le_bytes());
            data.extend_from_slice(&0u16.to_le_bytes());
            data.extend_from_slice(&0u16.to_le_bytes());
            data.extend_from_slice(&crc.to_le_bytes());
            data.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            data.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            data.extend_from_slice(&(name.len() as u16).to_le_bytes());
            data.extend_from_slice(&0u16.to_le_bytes());
            data.extend_from_slice(name.as_bytes());
            data.extend_from_slice(payload);
            central.extend_from_slice(b"PK\x01\x02");
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            central.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u32.to_le_bytes());
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        let central_start = data.len() as u32;
        data.extend_from_slice(&central);
        data.extend_from_slice(b"PK\x05\x06");
        data.extend_from_slice(&0u16.to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes());
        data.extend_from_slice(&(files.len() as u16).to_le_bytes());
        data.extend_from_slice(&(files.len() as u16).to_le_bytes());
        data.extend_from_slice(&(central.len() as u32).to_le_bytes());
        data.extend_from_slice(&central_start.to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes());
        Archive::parse(data).expect("the fixture archive parses")
    }

    const OPF: &str = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>Wicked &amp; the Witch</dc:title>
    <dc:creator>Gregory Maguire</dc:creator>
    <dc:language>en</dc:language>
    <dc:identifier id="id">urn:isbn:9780061970020</dc:identifier>
    <meta name="cover" content="cover-img"/>
  </metadata>
  <manifest>
    <item id="cover-img" href="images/cover.jpg" properties="cover-image"/>
    <item id="nav" href="nav.xhtml" properties="nav"/>
    <item id="c1" href="text/ch1.xhtml" media-type="application/xhtml+xml"/>
    <item id="c2" href="text/ch2.xhtml" media-type="application/xhtml+xml"/>
    <item id="css" href="css/main.css" media-type="text/css"/>
  </manifest>
  <spine>
    <itemref idref="c1"/>
    <itemref idref="c2"/>
  </spine>
</package>"#;

    const NAV: &str = r#"<html><body>
<nav epub:type="toc"><ol>
  <li><a href="ch1.xhtml">One &#8212; Munchkinlanders</a></li>
  <li><ol>
    <li><a href="ch2.xhtml">Two</a></li>
  </ol></li>
</ol></nav></body></html>"#;

    fn book() -> Archive {
        archive_with(&[
            (
                "META-INF/container.xml",
                r#"<container><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>"#,
            ),
            ("OEBPS/content.opf", OPF),
            ("OEBPS/nav.xhtml", NAV),
            ("OEBPS/text/ch1.xhtml", "<p>chapter one</p>"),
            ("OEBPS/text/ch2.xhtml", "<p>chapter two</p>"),
            ("OEBPS/images/cover.jpg", "not-really-a-jpeg"),
        ])
    }

    #[test]
    fn parses_metadata_spine_and_toc() {
        let info = parse(&book()).expect("parse");
        assert_eq!(info.title.as_deref(), Some("Wicked & the Witch"));
        assert_eq!(info.creator.as_deref(), Some("Gregory Maguire"));
        assert_eq!(info.language.as_deref(), Some("en"));
        assert_eq!(info.identifier.as_deref(), Some("urn:isbn:9780061970020"));
        assert_eq!(
            info.spine,
            vec!["OEBPS/text/ch1.xhtml", "OEBPS/text/ch2.xhtml"]
        );
        assert_eq!(info.cover.as_deref(), Some("OEBPS/images/cover.jpg"));
    }

    #[test]
    fn the_toc_resolves_against_the_nav_directory_and_keeps_depth() {
        let info = parse(&book()).expect("parse");
        assert_eq!(info.toc.len(), 2, "{:?}", info.toc);
        assert_eq!(info.toc[0].label, "One — Munchkinlanders");
        assert_eq!(info.toc[0].href, "OEBPS/text/ch1.xhtml");
        assert_eq!(info.toc[0].spine_index, Some(0));
        assert_eq!(info.toc[0].depth, 0);
        assert_eq!(info.toc[1].depth, 1, "nested ol is depth 1");
        assert_eq!(info.toc[1].spine_index, Some(1));
    }

    #[test]
    fn hrefs_resolve_relative_to_the_opf_not_the_root() {
        // The trap: a naive join produces "OEBPS/OEBPS/text/ch1.xhtml".
        let info = parse(&book()).expect("parse");
        assert!(info.spine.iter().all(|p| !p.contains("OEBPS/OEBPS")));
    }

    #[test]
    fn a_parent_relative_href_resolves() {
        let archive = archive_with(&[
            (
                "META-INF/container.xml",
                r#"<container><rootfiles><rootfile full-path="a/b/content.opf"/></rootfiles></container>"#,
            ),
            (
                "a/b/content.opf",
                r#"<package><manifest>
                    <item id="c" href="../c1.xhtml" media-type="application/xhtml+xml"/>
                 </manifest><spine><itemref idref="c"/></spine></package>"#,
            ),
            ("a/c1.xhtml", "<p/>"),
        ]);
        let info = parse(&archive).expect("parse");
        assert_eq!(info.spine, vec!["a/c1.xhtml"]);
    }

    #[test]
    fn an_epub2_ncx_is_the_toc_when_there_is_no_nav() {
        let archive = archive_with(&[
            (
                "META-INF/container.xml",
                r#"<container><rootfiles><rootfile full-path="book.opf"/></rootfiles></container>"#,
            ),
            (
                "book.opf",
                r#"<package xmlns="http://www.idpf.org/2007/opf" version="2.0">
                  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
                    <dc:title>Old Book</dc:title>
                  </metadata>
                  <manifest>
                    <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
                    <item id="c1" href="one.html" media-type="application/xhtml+xml"/>
                    <item id="c2" href="two.html" media-type="application/xhtml+xml"/>
                  </manifest>
                  <spine toc="ncx"><itemref idref="c1"/><itemref idref="c2"/></spine>
                </package>"#,
            ),
            (
                "toc.ncx",
                r#"<ncx><navMap>
                  <navPoint id="n1"><navLabel><text>Start</text></navLabel><content src="one.html#s1"/></navPoint>
                  <navPoint id="n2"><navLabel><text>Later</text></navLabel><content src="two.html"/></navPoint>
                </navMap></ncx>"#,
            ),
            ("one.html", "<p/>"),
            ("two.html", "<p/>"),
        ]);
        let info = parse(&archive).expect("parse");
        assert_eq!(info.title.as_deref(), Some("Old Book"));
        assert_eq!(info.toc.len(), 2);
        assert_eq!(info.toc[0].label, "Start");
        // The fragment is stripped, so the entry matches a spine item.
        assert_eq!(info.toc[0].href, "one.html");
        assert_eq!(info.toc[0].spine_index, Some(0));
        assert_eq!(info.toc[1].spine_index, Some(1));
    }

    #[test]
    fn a_missing_container_is_named() {
        let archive = archive_with(&[("book.opf", "<package/>")]);
        let err = parse(&archive).unwrap_err();
        assert!(matches!(err, EpubError::MissingContainer), "{err}");
    }

    #[test]
    fn a_missing_opf_is_named_with_its_path() {
        let archive = archive_with(&[
            (
                "META-INF/container.xml",
                r#"<container><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>"#,
            ),
        ]);
        let err = parse(&archive).unwrap_err();
        match err {
            EpubError::MissingOpf { path } => assert_eq!(path, "OEBPS/content.opf"),
            other => panic!("{other}"),
        }
    }

    #[test]
    fn a_gutenberg_style_cover_meta_resolves_to_a_manifest_item() {
        let archive = archive_with(&[
            (
                "META-INF/container.xml",
                r#"<container><rootfiles><rootfile full-path="content.opf"/></rootfiles></container>"#,
            ),
            (
                "content.opf",
                r#"<package>
                  <metadata>
                    <meta name="cover" content="coverimage"/>
                  </metadata>
                  <manifest>
                    <item id="coverimage" href="cover.jpg"/>
                    <item id="c1" href="c1.html"/>
                  </manifest>
                  <spine><itemref idref="c1"/></spine>
                </package>"#,
            ),
            ("cover.jpg", "jpg"),
            ("c1.html", "<p/>"),
        ]);
        let info = parse(&archive).expect("parse");
        assert_eq!(info.cover.as_deref(), Some("cover.jpg"));
    }

    #[test]
    fn percent_encoded_hrefs_are_decoded() {
        let archive = archive_with(&[
            (
                "META-INF/container.xml",
                r#"<container><rootfiles><rootfile full-path="content.opf"/></rootfiles></container>"#,
            ),
            (
                "content.opf",
                "<package><manifest><item id=\"c\" href=\"chapter%20one.xhtml\"/></manifest>\
                 <spine><itemref idref=\"c\"/></spine></package>",
            ),
            ("chapter one.xhtml", "<p/>"),
        ]);
        let info = parse(&archive).expect("parse");
        assert_eq!(info.spine, vec!["chapter one.xhtml"]);
    }

    #[test]
    fn resolve_handles_dotdot_and_dot() {
        assert_eq!(resolve("OEBPS", "text/c1.xhtml"), "OEBPS/text/c1.xhtml");
        assert_eq!(resolve("OEBPS", "../c1.xhtml"), "c1.xhtml");
        assert_eq!(resolve("OEBPS", "./c1.xhtml"), "OEBPS/c1.xhtml");
        assert_eq!(resolve("", "c1.xhtml"), "c1.xhtml");
    }

    // Security-review test (2026-10-30): a multibyte char right
    // after an incomplete escape used to panic the original slice; now it
    // degrades to the literal bytes.
    #[test]
    fn a_multibyte_char_after_a_stray_percent_does_not_panic() {
        assert_eq!(percent_decode("%Aé"), "%Aé");
        assert_eq!(percent_decode("%\u{00E9}x"), "%\u{00E9}x");
        assert_eq!(percent_decode("%41é"), "Aé");
        assert_eq!(percent_decode("plain"), "plain");
    }

    #[test]
    fn decode_entities_covers_the_named_set() {
        assert_eq!(decode_entities("&amp;&lt;&gt;&quot;&apos;").unwrap(), "&<>\"'");
        assert_eq!(decode_entities("&#8212;").unwrap(), "—");
        assert_eq!(decode_entities("&#x2014;").unwrap(), "—");
        assert_eq!(decode_entities("plain").unwrap(), "plain");
    }
}
