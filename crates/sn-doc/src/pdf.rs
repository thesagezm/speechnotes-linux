//! PDF: the container walk, the outline, and the metadata the shelf needs.
//!
//! A PDF is a body of numbered *objects* with a cross-reference table saying
//! where each one starts and a trailer saying where the table is. Everything
//! the shelf shows — title, author, page count, cover, chapter outline — is
//! reachable from the trailer's `/Root` without touching a page's content,
//! which is why this module exists: a shelf of books must cost one box walk,
//! not a renderer.
//!
//! The traps, all of which real books set:
//!
//! * **The xref table lies about offsets.** A linearised or incrementally
//!   updated file's table can be stale after an append. The recovery rule
//!   follows the spec: if a `startxref` offset does not hold a table, scan
//!   backwards for the nearest `startxref` keyword and try again, and fall
//!   back to rebuilding the table by walking the body for `N G obj`.
//! * **Object streams exist.** PDF 1.5+ packs objects into `/ObjStm`
//!   containers, compressed with FlateDecode and indexed by `/First` and
//!   `/Extends`. The page count and the outline often live in there, so the
//!   walk resolves a compressed object by parsing its stream's pairs.
//! * **Encryption is detected, never defeated.** `/Encrypt` in the trailer
//!   means the file is encrypted; this module reports which handler and
//!   refuses to read the strings. Decrypting someone's document is not the
//!   library's job.
//! * **Page counts come from the page tree, not from a divisor.** A
//!   `/Count` that disagrees with the tree is the book's lie; the tree is
//!   walked, `/Count` is only a hint for allocation.
//! * **Streams carry filters, and `/Filter` wears two shapes.** A name
//!   (`/FlateDecode`) or an array of them (`[/FlateDecode]`, `[/ASCII85Decode
//!   /FlateDecode]`) — one real book writes nothing but the array form, and
//!   a name-only reading turned every content stream of that book into raw
//!   compressed bytes and its text layer into rubble. [`Document::stream_data`]
//!   opens the text-layer filters (Flate, LZW, ASCIIHex, ASCII85, RunLength,
//!   with their predictors) in chain order and passes image and font-program
//!   filters through raw.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

/// What the container says about a document.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PdfInfo {
    /// The document title, from `/Info /Title` or the catalog's `/Title`.
    pub title: Option<String>,
    /// The author, from `/Info /Author`.
    pub author: Option<String>,
    /// The producer, from `/Info /Producer` — the honest answer about what
    /// wrote the file.
    pub producer: Option<String>,
    /// The PDF version string from the header (`%PDF-1.7` → "1.7").
    pub version: Option<String>,
    /// Pages counted by walking the page tree.
    pub page_count: usize,
    /// The outline (table of contents), flattened with page numbers.
    pub outline: Vec<OutlineEntry>,
    /// The first page's text, when a text layer exists. `None` for a scan.
    pub first_page_text: Option<String>,
    /// `/Encrypt` was present: the file is encrypted and its strings were
    /// not read.
    pub encrypted: bool,
    /// The encryption handler's name (`/Standard`, `/AESV2`, …) when
    /// encrypted.
    pub encryption_filter: Option<String>,
    /// A page's width and height in points, from the first leaf.
    pub page_size: Option<(f64, f64)>,
}

/// One row of the outline, with the page it points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineEntry {
    pub title: String,
    /// The 1-based page the destination resolves to, when it resolves.
    pub page: Option<usize>,
    /// Nesting depth: 0 is a top-level chapter.
    pub depth: usize,
}

/// How many outline rows the shelf will flatten. A book with a pathological
/// contents tree must not turn the shelf into a hang; a thousand rows is
/// every contents list anyone has ever shipped.
const OUTLINE_BUDGET: usize = 1000;

/// How many link annotations the viewer will carry. A scanned page can
/// carry hundreds; a whole book in the tens of thousands — anything past
/// this is a malformed file, not a table of contents.
const LINK_BUDGET: usize = 100_000;

/// One link annotation on one page: where it sits and where it goes.
#[derive(Debug, Clone, PartialEq)]
pub struct PageLink {
    /// The 1-based page the link sits on.
    pub page: usize,
    /// The clickable rectangle, `(x0, y0, x1, y1)` in points, with the
    /// page's **top-left** as the origin — PDF rects run from the
    /// bottom-left, and this is normalised to the same top-left the
    /// rendered image has, so the view multiplies by its scale alone.
    pub rect: (f64, f64, f64, f64),
    /// Where the link goes.
    pub target: PageLinkTarget,
}

/// Where a link annotation points.
#[derive(Debug, Clone, PartialEq)]
pub enum PageLinkTarget {
    /// A page in this document, 1-based — a `/GoTo` destination. This is
    /// the book's own contents page: the row the reader clicks jumps here.
    Page(usize),
    /// An external address (`/S /URI`) — the desktop's safe-link rule
    /// decides whether it opens.
    Uri(String),
}

/// Why a PDF could not be read.
#[derive(Debug, thiserror::Error)]
pub enum PdfError {
    #[error("not a pdf: no %PDF- header")]
    NotAPdf,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("the cross-reference table could not be found or rebuilt")]
    NoXref,
    #[error("corrupt pdf: {0}")]
    Corrupt(String),
    #[error("the pdf is encrypted ({filter}); its text cannot be read")]
    Encrypted { filter: String },
}

/// Read the container's metadata: everything except page contents.
///
/// This is the shelf's call. It walks the trailer, resolves the catalog,
/// counts pages through the tree, flattens the outline, and stops.
pub fn parse(bytes: &[u8]) -> Result<PdfInfo, PdfError> {
    let doc = Document::parse(bytes)?;
    doc.info()
}

/// A parsed document: the object table, resolved enough to answer the
/// shelf's questions.
pub struct Document {
    data: Vec<u8>,
    objects: HashMap<u32, Obj>,
}

/// One PDF object: the subset this crate needs.
#[derive(Debug, Clone, PartialEq)]
pub enum Obj {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    Str(Vec<u8>),
    Name(String),
    Array(Vec<Obj>),
    Dict(HashMap<String, Obj>),
    Ref(u32),
    /// A stream object: its dictionary plus its (still compressed) bytes.
    Stream { dict: HashMap<String, Obj>, data: Vec<u8> },
}

impl Obj {
    /// The dictionary behind this object, following a reference.
    pub fn as_dict<'a>(&'a self, doc: &'a Document) -> Option<&'a HashMap<String, Obj>> {
        match self {
            Obj::Dict(d) => Some(d),
            Obj::Ref(id) => doc.objects.get(id).and_then(|o| o.as_dict(doc)),
            _ => None,
        }
    }
    /// The array behind this object, following a reference.
    pub fn as_array<'a>(&'a self, doc: &'a Document) -> Option<&'a Vec<Obj>> {
        match self {
            Obj::Array(a) => Some(a),
            Obj::Ref(id) => doc.objects.get(id).and_then(|o| o.as_array(doc)),
            _ => None,
        }
    }
    /// A name's value, or `None` when this is not a name.
    pub fn as_name(&self) -> Option<&str> {
        match self {
            Obj::Name(n) => Some(n.as_str()),
            _ => None,
        }
    }
    /// An integer, following a reference. Reals truncate.
    pub fn as_int(&self, doc: &Document) -> Option<i64> {
        match self {
            Obj::Int(i) => Some(*i),
            Obj::Real(f) => Some(*f as i64),
            Obj::Ref(id) => doc.objects.get(id).and_then(|o| o.as_int(doc)),
            _ => None,
        }
    }
    /// A text string, decoded as UTF-8 (with a Latin-1 fallback) and with a
    /// UTF-16BE BOM honoured.
    pub fn as_text(&self, doc: &Document) -> Option<String> {
        let raw = match self {
            Obj::Str(s) => s,
            Obj::Ref(id) => return doc.objects.get(id).and_then(|o| o.as_text(doc)),
            _ => return None,
        };
        Some(decode_text_string(raw))
    }
}

impl Document {
    /// The file's raw bytes, as parsed — the pdfium glyph source re-opens
    /// them.
    //  the original pdf.rs predates the pdfium glyph source, whose
    // original pdfium_source.rs calls `doc.raw_bytes()`; this accessor is the
    // certain reading of that call (the parse keeps the original bytes).
    pub fn raw_bytes(&self) -> &[u8] {
        &self.data
    }

    /// Parse a whole file's bytes.
    pub fn parse(bytes: &[u8]) -> Result<Self, PdfError> {
        let header = bytes.windows(8).take(bytes.len().min(4096)).find_map(|w| {
            std::str::from_utf8(w)
                .ok()
                .filter(|s| s.starts_with("%PDF-"))
                .map(|s| s.to_string())
        });
        if header.is_none() {
            return Err(PdfError::NotAPdf);
        }
        let objects = rebuild_xref(bytes)?;
        Ok(Self { data: bytes.to_vec(), objects })
    }

    /// How many objects the rebuilt table holds. Diagnostics only.
    pub fn object_count(&self) -> usize {
        self.objects.len()
    }

    /// Resolve a reference to its object.
    pub fn get(&self, id: u32) -> Option<&Obj> {
        self.objects.get(&id)
    }

    /// A stream's decoded bytes: the filters a text layer meets (Flate,
    /// LZW, ASCIIHex, ASCII85, RunLength) are opened; image and font-program
    /// filters (DCTDecode, JPXDecode, CCITTFaxDecode, JBIG2Decode) are
    /// returned raw and the consumer decides.
    ///
    /// `/Filter` may be a name, an array of names, or an indirect reference
    /// to either — many producers write the array form (`[/FlateDecode]`),
    /// and one real book carries nothing but that form, which is why the
    /// name-only reading turned every content stream of that book into raw
    /// compressed bytes and its text layer into rubble. A chain is applied
    /// in order; `/DecodeParms` (`/DP`) is either one dict for a
    /// single-filter stream or an array parallel to `/Filter`.
    pub fn stream_data(&self, obj: &Obj) -> Option<Vec<u8>> {
        match obj {
            Obj::Stream { dict, data } => self.decode_stream(dict, data),
            Obj::Ref(id) => self.get(*id).and_then(|o| self.stream_data(o)),
            _ => None,
        }
    }

    /// The filter names a stream dict carries, in the order each was
    /// applied, references resolved.
    fn filter_chain(&self, dict: &HashMap<String, Obj>) -> Vec<String> {
        match dict.get("Filter") {
            Some(Obj::Name(n)) => vec![n.clone()],
            Some(Obj::Array(items)) => items
                .iter()
                .filter_map(|o| match o {
                    Obj::Name(n) => Some(n.clone()),
                    Obj::Ref(id) => self.get(*id).and_then(|r| r.as_name()).map(String::from),
                    _ => None,
                })
                .collect(),
            Some(o @ Obj::Ref(_)) => {
                // An indirect dict that itself carries /Filter, or an
                // indirect name (`/Filter 12 0 R` naming a name).
                match self.dict_of_obj(o).and_then(|d| d.get("Filter")) {
                    Some(f) => f
                        .as_name()
                        .map(|n| vec![n.to_string()])
                        .unwrap_or_default(),
                    None => self
                        .get(o.as_int(self).unwrap_or(0) as u32)
                        .and_then(|r| r.as_name())
                        .map(|n| vec![n.to_string()])
                        .unwrap_or_default(),
                }
            }
            _ => Vec::new(),
        }
    }

    /// The decode parameters for chain step `i`: a single dict applies only
    /// to the first (and normally only) filter; the array form is parallel
    /// to `/Filter`.
    fn decode_parms<'a>(
        &'a self,
        dict: &'a HashMap<String, Obj>,
        index: usize,
    ) -> Option<&'a HashMap<String, Obj>> {
        let value = dict.get("DecodeParms").or_else(|| dict.get("DP"))?;
        match value {
            Obj::Array(items) => self.dict_of_obj(items.get(index)?),
            _ => {
                if index == 0 {
                    self.dict_of_obj(value)
                } else {
                    None
                }
            }
        }
    }

    /// Decode one stream through its filter chain. A failed step ends the
    /// chain honestly (`None`) — half-decoded bytes are worse than nothing —
    /// and an unopenable terminal filter (an image) returns raw, the way the
    /// name-only reading always did.
    fn decode_stream(&self, dict: &HashMap<String, Obj>, data: &[u8]) -> Option<Vec<u8>> {
        let chain = self.filter_chain(dict);
        if chain.is_empty() {
            return Some(data.to_vec());
        }
        let mut out = Some(data.to_vec());
        for (i, name) in chain.iter().enumerate() {
            let cur = out?;
            out = match name.as_str() {
                "FlateDecode" | "Fl" => {
                    let inflated = inflate(&cur)?;
                    Some(self.unpredict(&inflated, self.decode_parms(dict, i)))
                }
                "LZWDecode" | "LZW" => {
                    let early = self
                        .decode_parms(dict, i)
                        .and_then(|p| p.get("EarlyChange"))
                        .and_then(|o| o.as_int(self))
                        .unwrap_or(1);
                    let decoded = lzw_decode(&cur, early != 0)?;
                    Some(self.unpredict(&decoded, self.decode_parms(dict, i)))
                }
                "ASCIIHexDecode" | "AHx" => Some(asciihex_decode(&cur)),
                "ASCII85Decode" | "A85" => ascii85_decode(&cur),
                "RunLengthDecode" | "RL" => Some(runlength_decode(&cur)),
                _ => Some(cur),
            };
        }
        out
    }

    /// Undo a stream predictor: PNG predictors (10–15) wrap every row in a
    /// filter byte; the row width is Columns × Colors × BitsPerComponent
    /// rounded up to a byte — what makes the /ObjStm header pairs of an
    /// xref-stream file readable. Other predictors (TIFF 2) are not openable
    /// here — the decoded bytes pass through unchanged.
    fn unpredict(&self, data: &[u8], parms: Option<&HashMap<String, Obj>>) -> Vec<u8> {
        let predictor = parms
            .and_then(|p| p.get("Predictor"))
            .and_then(|o| o.as_int(self))
            .unwrap_or(1);
        if !(10..=15).contains(&predictor) {
            return data.to_vec();
        }
        let columns = parms
            .and_then(|p| p.get("Columns"))
            .and_then(|o| o.as_int(self))
            .unwrap_or(1)
            .max(1) as usize;
        let colors = parms
            .and_then(|p| p.get("Colors"))
            .and_then(|o| o.as_int(self))
            .unwrap_or(1)
            .max(1) as usize;
        let bits = parms
            .and_then(|p| p.get("BitsPerComponent"))
            .and_then(|o| o.as_int(self))
            .unwrap_or(8)
            .max(1) as usize;
        let row_width = (colors * bits * columns).div_ceil(8);
        unpredict_png(data, row_width)
    }

    /// The dictionary a value names directly (no key hop).
    fn dict_of_obj<'a>(&'a self, obj: &'a Obj) -> Option<&'a HashMap<String, Obj>> {
        match obj {
            Obj::Dict(d) => Some(d),
            Obj::Ref(id) => self.get(*id).and_then(|o| o.as_dict(self)),
            _ => None,
        }
    }

    /// A dictionary's value as a name, following references.
    pub fn name_of<'a>(&'a self, dict: &'a HashMap<String, Obj>, key: &str) -> Option<&'a str> {
        self.lookup(dict, key).and_then(|o| o.as_name())
    }

    /// A dictionary's value as an integer, following references.
    pub fn int_of(&self, dict: &HashMap<String, Obj>, key: &str) -> Option<i64> {
        self.lookup(dict, key).and_then(|o| o.as_int(self))
    }

    /// A dictionary's value as a real, following references.
    pub fn real_of(&self, dict: &HashMap<String, Obj>, key: &str) -> Option<f64> {
        self.lookup(dict, key).and_then(|o| o.as_real(self))
    }

    /// The array behind a dictionary key, following references.
    pub fn array_of<'a>(
        &'a self,
        dict: &'a HashMap<String, Obj>,
        key: &str,
    ) -> Option<&'a Vec<Obj>> {
        self.lookup(dict, key).and_then(|o| o.as_array(self))
    }

    /// A dictionary's sub-dictionary, following references.
    pub fn dict_of<'a>(
        &'a self,
        dict: &'a HashMap<String, Obj>,
        key: &str,
    ) -> Option<&'a HashMap<String, Obj>> {
        self.lookup(dict, key).and_then(|o| o.as_dict(self))
    }

    /// The page tree's leaf objects, in reading order, with their ids.
    pub fn pages(&self) -> Vec<(u32, &HashMap<String, Obj>)> {
        let Some(catalog) = self.catalog() else {
            return Vec::new();
        };
        let Some(pages_root) = self.dict_of(catalog, "Pages") else {
            return Vec::new();
        };
        let mut ids = Vec::new();
        self.collect_leaves(pages_root, &mut ids);
        ids.into_iter()
            .filter_map(|id| {
                self.get(id)
                    .and_then(|o| o.as_dict(self))
                    .map(|d| (id, d))
            })
            .collect()
    }

    /// Follow a chain of dictionary keys to an object.
    pub fn lookup<'a>(&'a self, dict: &'a HashMap<String, Obj>, key: &str) -> Option<&'a Obj> {
        let found = dict.get(key)?;
        match found {
            Obj::Ref(id) => self.objects.get(id),
            other => Some(other),
        }
    }

    /// The trailer's catalog (`/Root`).
    fn catalog(&self) -> Option<&HashMap<String, Obj>> {
        // The trailer is the last `trailer` keyword's dictionary, or the
        // cross-reference stream's dictionary in a 1.5+ file. The rebuilt
        // table records whichever one it found as object 0's place: the
        // trailer's `/Root` reference is followed from here.
        let root = self.trailer()?;
        self.lookup(root, "Root").and_then(|o| o.as_dict(self))
    }

    /// The object whose `/Type` is `/Catalog` — the fallback when the
    /// trailer does not name `/Root`. Owned, because it may come from any
    /// of the table's objects.
    fn find_catalog_by_type(&self) -> Option<HashMap<String, Obj>> {
        self.objects.values().find_map(|o| match o {
            Obj::Dict(d) if d.get("Type").and_then(|t| t.as_name()) == Some("Catalog") => {
                Some(d.clone())
            }
            _ => None,
        })
    }

    /// The trailer dictionary, rebuilt from the last `trailer` keyword or
    /// the last xref stream.
    fn trailer(&self) -> Option<&HashMap<String, Obj>> {
        self.objects.get(&0).and_then(|o| o.as_dict(self))
    }

    /// What the container says about the document.
    pub fn info(&self) -> Result<PdfInfo, PdfError> {
        let mut info = PdfInfo::default();
        info.version = self.version();

        // The trailer's `/Info` is a reference to the info dictionary; an
        // incremental update can leave several, and the last one wins.
        if let Some(trailer) = self.trailer() {
            if !trailer.contains_key("Encrypt") {
                if let Some(info_dict) = self.lookup(trailer, "Info").and_then(|o| o.as_dict(self)) {
                    info.title = self.lookup(info_dict, "Title").and_then(|o| o.as_text(self));
                    info.author = self.lookup(info_dict, "Author").and_then(|o| o.as_text(self));
                    info.producer = self.lookup(info_dict, "Producer").and_then(|o| o.as_text(self));
                }
            }
            // /Encrypt may be present but unresolvable (the trailer often
            // points at an object outside this walk). Presence is the fact
            // that matters: the strings were not read either way.
            let has_encrypt = match trailer.get("Encrypt") {
                Some(_) => true,
                None => false,
            };
            if has_encrypt {
                info.encrypted = true;
                info.encryption_filter = self
                    .lookup(trailer, "Encrypt")
                    .and_then(|o| o.as_dict(self))
                    .and_then(|d| self.lookup(d, "Filter"))
                    .and_then(|o| o.as_name())
                    .map(str::to_string);
                // An encrypted document's strings are ciphertext; reading
                // them would put garbage on the shelf. The outline and the
                // page count are still walkable (the numbers are not
                // encrypted), so those are answered honestly.
                info.title = None;
                info.author = None;
                info.producer = None;
            }
        }

        if std::env::var("SN_PDF_TRACE").map(|v| !v.is_empty()).unwrap_or(false) {
            eprintln!("TRACE info: trailer walked");
        }
        // The trailer usually names /Root, but a written-by-hand file can
        // omit it (one on the user's Desktop does). The catalog is still
        // findable: it is the object whose /Type is /Catalog.
        let catalog_owned;
        let catalog: &HashMap<String, Obj> = match self.catalog() {
            Some(c) => c,
            None => match self.find_catalog_by_type() {
                Some(c) => {
                    catalog_owned = c;
                    &catalog_owned
                }
                None => return Ok(info),
            },
        };
        if std::env::var("SN_PDF_TRACE").map(|v| !v.is_empty()).unwrap_or(false) {
            eprintln!("TRACE info: catalog found");
        }
        // A title may live in the catalog's metadata when /Info has none.
        if info.title.is_none() {
            info.title = self.lookup(catalog, "Title").and_then(|o| o.as_text(self));
        }

        // The page tree: /Root /Pages, whose /Kids are leaves and branches.
        if let Some(pages) = self.lookup(catalog, "Pages").and_then(|o| o.as_dict(self)) {
            let mut visited: Vec<u32> = Vec::new();
            let (count, first_size) = self.walk_pages(pages, &mut visited);
            info.page_count = count;
            info.page_size = first_size;
        }
        if std::env::var("SN_PDF_TRACE").map(|v| !v.is_empty()).unwrap_or(false) {
            eprintln!("TRACE info: pages walked = {}", info.page_count);
        }

        // The outline: /Root /Outlines with nested /First/ /Next children.
        if let Some(outlines) = self.lookup(catalog, "Outlines").and_then(|o| o.as_dict(self)) {
            let mut pages: Vec<u32> = Vec::new();
            if let Some(pages_dict) = self.lookup(catalog, "Pages").and_then(|o| o.as_dict(self)) {
                self.collect_leaves(pages_dict, &mut pages);
            }
            if let Some(first) = self.lookup(outlines, "First") {
                let mut budget: usize = OUTLINE_BUDGET;
                self.walk_outline(first, 0, &pages, &mut info.outline, &mut budget);
            }
        }
        if std::env::var("SN_PDF_TRACE").map(|v| !v.is_empty()).unwrap_or(false) {
            eprintln!("TRACE info: outline walked = {}", info.outline.len());
        }

        Ok(info)
    }

    /// The PDF version from the header.
    fn version(&self) -> Option<String> {
        let head = &self.data[..self.data.len().min(64)];
        let text = String::from_utf8_lossy(head);
        text.find("%PDF-").and_then(|i| {
            text[i + 5..]
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect::<String>()
                .split('.')
                .take(2)
                .map(|s| s.to_string())
                .reduce(|a, b| format!("{a}.{b}"))
        })
    }

    /// Walk the page tree, counting leaves and taking the first leaf's box.
    ///
    /// A `/Count` that disagrees with the tree is the book's lie, so the
    /// tree is walked and `/Count` is never trusted for the answer. Cycles
    /// are guarded by `visited`: a `/Parent` or `/Kids` pointing back at a
    /// node already on the path would otherwise recurse until the stack ran
    /// out, which is what a hand-edited or truncated book produces.
    fn walk_pages(
        &self,
        node: &HashMap<String, Obj>,
        visited: &mut Vec<u32>,
    ) -> (usize, Option<(f64, f64)>) {
        let kids = match self.lookup(node, "Kids").and_then(|o| o.as_array(self)) {
            Some(k) => k.clone(),
            None => return (0, None),
        };
        let mut count = 0;
        let mut first = None;
        for kid in kids {
            let Obj::Ref(id) = kid else { continue };
            if visited.contains(&id) {
                continue;
            }
            visited.push(id);
            let Some(kid_dict) = self.objects.get(&id).and_then(|o| o.as_dict(self)) else {
                continue;
            };
            let is_branch = self.lookup(kid_dict, "Kids").is_some();
            if is_branch {
                let owned: HashMap<String, Obj> = kid_dict.clone();
                let (n, f) = self.walk_pages(&owned, visited);
                count += n;
                if first.is_none() {
                    first = f;
                }
            } else {
                count += 1;
                if first.is_none() {
                    first = self.leaf_size(kid_dict);
                }
            }
        }
        (count, first)
    }

    /// A leaf's `/MediaBox`, defaulted to US Letter when absent.
    fn leaf_size(&self, leaf: &HashMap<String, Obj>) -> Option<(f64, f64)> {
        let (x0, y0, x1, y1) = self.leaf_box(leaf)?;
        Some((x1 - x0, y1 - y0))
    }

    /// A leaf's full `/MediaBox`: `(x0, y0, x1, y1)` in points. The link
    /// rectangles are relative to the box's own corner, which is usually
    /// the origin but need not be.
    fn leaf_box(&self, leaf: &HashMap<String, Obj>) -> Option<(f64, f64, f64, f64)> {
        let box_ = self.lookup(leaf, "MediaBox").and_then(|o| o.as_array(self))?;
        if box_.len() < 4 {
            return None;
        }
        let x0 = box_[0].as_real(self)?;
        let y0 = box_[1].as_real(self)?;
        let x1 = box_[2].as_real(self)?;
        let y1 = box_[3].as_real(self)?;
        Some((x0, y0, x1, y1))
    }

    /// One (width, height) in points per page, reading order; a page whose
    /// tree carries no readable `/MediaBox` is left out (the shelf then
    /// falls back to a letter-sized grid for the whole document).
    //  authored from the call site (sn-store books.rs
    // `doc.page_sizes() -> Vec<(f64, f64)>` with its own empty-vec fallback);
    // no original of this function surfaced. Reuses the file's own `leaf_size`.
    pub fn page_sizes(&self) -> Vec<(f64, f64)> {
        self.pages()
            .iter()
            .filter_map(|(_, dict)| self.leaf_size(dict))
            .collect()
    }

    /// Every link annotation on every page, in reading order.
    ///
    /// A page's `/Annots` array carries its annotations; a link is the one
    /// whose `/Subtype` is `/Link`, and its rectangle is where it sits. The
    /// destination is whatever the outline machinery already reads — the
    /// same `/Dest` array or `/A << /S /GoTo /D … >>` action — so a
    /// contents-page row and an outline row resolve identically. A `/S
    /// /URI` action is the web's kind of link and is reported as such.
    ///
    /// A page with no annotations, or an annotation the reader cannot
    /// resolve a target for (a named destination the container walk does
    /// not build, a border without an action), is simply left out.
    pub fn page_links(&self) -> Vec<PageLink> {
        let pages = self.pages();
        let page_ids: Vec<u32> = pages.iter().map(|(id, _)| *id).collect();
        let mut out = Vec::new();
        for (page_index, (_, dict)) in pages.iter().enumerate() {
            let page = page_index + 1;
            let Some((bx0, _, _, by1)) = self.leaf_box(dict) else {
                continue;
            };
            let Some(annots) = self.lookup(dict, "Annots").and_then(|o| o.as_array(self)) else {
                continue;
            };
            let annots = annots.clone();
            for annot in &annots {
                if out.len() >= LINK_BUDGET {
                    return out;
                }
                let Some(annot) = annot.as_dict(self) else {
                    continue;
                };
                // Only links: stamps, highlights and form fields are not
                // the book's contents.
                if !matches!(self.lookup(annot, "Subtype"), Some(Obj::Name(n)) if n == "Link") {
                    continue;
                }
                let Some(rect) = self.normalised_link_rect(annot, bx0, by1) else {
                    continue;
                };
                if let Some(target) = self.link_target(annot, &page_ids) {
                    out.push(PageLink { page, rect, target });
                }
            }
        }
        out
    }

    /// A link's `/Rect`, normalised from PDF's bottom-left user space to
    /// the page's top-left, and sorted so x0 <= x1 and y0 <= y1 — a writer
    /// may emit the corners in either order.
    fn normalised_link_rect(
        &self,
        annot: &HashMap<String, Obj>,
        box_x0: f64,
        box_y1: f64,
    ) -> Option<(f64, f64, f64, f64)> {
        let rect = self.lookup(annot, "Rect").and_then(|o| o.as_array(self))?;
        if rect.len() < 4 {
            return None;
        }
        let rx0 = rect[0].as_real(self)?;
        let ry0 = rect[1].as_real(self)?;
        let rx1 = rect[2].as_real(self)?;
        let ry1 = rect[3].as_real(self)?;
        let (left, right) = rx0.partial_cmp(&rx1).map_or((rx0, rx1), |ord| {
            if ord == std::cmp::Ordering::Less { (rx0, rx1) } else { (rx1, rx0) }
        });
        // y runs up in PDF and down on the rendered page: subtract from the
        // box's own top.
        let (top, bottom) = if ry1 > ry0 { (ry1, ry0) } else { (ry0, ry1) };
        let y_top = box_y1 - top;
        let y_bottom = box_y1 - bottom;
        if !left.is_finite() || !y_top.is_finite() || !y_bottom.is_finite() {
            return None;
        }
        Some((left - box_x0, y_top, right - box_x0, y_bottom))
    }

    /// Where a link annotation goes: a page in this document, or a web
    /// address. The destination forms are the outline's own (`/Dest` array
    /// or `/A << /S /GoTo /D … >>`), so one read serves both.
    fn link_target(
        &self,
        annot: &HashMap<String, Obj>,
        page_ids: &[u32],
    ) -> Option<PageLinkTarget> {
        if let Some(action) = self.lookup(annot, "A").and_then(|o| o.as_dict(self)) {
            match self.lookup(action, "S") {
                Some(Obj::Name(name)) if name == "URI" => {
                    let uri = self
                        .lookup(action, "URI")
                        .and_then(|o| o.as_text(self))
                        .unwrap_or_default();
                    if uri.is_empty() {
                        return None;
                    }
                    return Some(PageLinkTarget::Uri(uri));
                }
                // GoTo falls through to the /D read below; anything else
                // (launch, JavaScript, …) is not this reader's business.
                _ => {}
            }
        }
        self.destination_page(annot, page_ids)
            .map(PageLinkTarget::Page)
    }

    /// Every leaf's object id, in reading order. The outline's destinations
    /// are page *references*, so a destination resolves to a 1-based page by
    /// its position in this list.
    ///
    /// Cycles are guarded by the `visited` set: a `/Parent` pointing back up
    /// the tree would otherwise recurse until the stack ran out.
    fn collect_leaves(&self, node: &HashMap<String, Obj>, out: &mut Vec<u32>) {
        self.collect_leaves_inner(node, out, &mut Vec::new());
    }

    fn collect_leaves_inner(
        &self,
        node: &HashMap<String, Obj>,
        out: &mut Vec<u32>,
        visited: &mut Vec<u32>,
    ) {
        let kids = match self.lookup(node, "Kids").and_then(|o| o.as_array(self)) {
            Some(k) => k.clone(),
            None => return,
        };
        for kid in kids {
            let Obj::Ref(id) = kid else { continue };
            if visited.contains(&id) {
                continue;
            }
            visited.push(id);
            let Some(kid_dict) = self.objects.get(&id).and_then(|o| o.as_dict(self)) else {
                continue;
            };
            if self.lookup(kid_dict, "Kids").is_some() {
                // A branch: recurse with the same guard.
                let owned: HashMap<String, Obj> = kid_dict.clone();
                self.collect_leaves_inner(&owned, out, visited);
            } else {
                out.push(id);
            }
        }
    }

    /// Walk the outline from one item, following `/First` (down) and
    /// `/Next` (across), recording each title's page.
    fn walk_outline(
        &self,
        item: &Obj,
        depth: usize,
        pages: &[u32],
        out: &mut Vec<OutlineEntry>,
        budget: &mut usize,
    ) {
        // A book with a pathological outline (thousands of nested entries)
        // must not turn the shelf into a hang; the first thousand rows is
        // every contents list anyone has.
        if *budget == 0 {
            return;
        }
        let mut current = Some(item.clone());
        while let Some(node) = current {
            *budget -= 1;
            if *budget == 0 {
                return;
            }
            let Some(dict) = node.as_dict(self) else { break };
            let title = self
                .lookup(dict, "Title")
                .and_then(|o| o.as_text(self))
                .unwrap_or_default();
            let page = self.destination_page(dict, pages);
            if !title.is_empty() || page.is_some() {
                out.push(OutlineEntry { title, page, depth });
            }
            // Down first: a chapter's sections belong under it.
            if let Some(first) = self.lookup(dict, "First") {
                self.walk_outline(first, depth + 1, pages, out, budget);
            }
            current = self.lookup(dict, "Next").cloned();
        }
    }

    /// The 1-based page an outline item's destination names.
    fn destination_page(&self, dict: &HashMap<String, Obj>, pages: &[u32]) -> Option<usize> {
        let dest = self.lookup(dict, "Dest").or_else(|| {
            // An action: /A << /S /GoTo /D [...] >>
            let action = self.lookup(dict, "A").and_then(|o| o.as_dict(self))?;
            self.lookup(action, "D")
        })?;
        match dest {
            Obj::Array(items) => self.page_of_dest(items, pages),
            Obj::Ref(id) => match self.objects.get(id) {
                Some(Obj::Array(items)) => self.page_of_dest(items, pages),
                _ => None,
            },
            Obj::Str(_) | Obj::Name(_) => {
                // A named destination: /Dest (section1) resolves through the
                // catalog's /Dests or the /Names tree, which this walk does
                // not build. The row is kept with no page.
                None
            }
            _ => None,
        }
    }

    /// `[/Page /XYZ x y z]` → the page's 1-based index.
    fn page_of_dest(&self, items: &[Obj], pages: &[u32]) -> Option<usize> {
        let target = items.first()?;
        match target {
            Obj::Ref(id) => pages.iter().position(|p| p == id).map(|i| i + 1),
            Obj::Int(index) => {
                // A page index, not a reference: the older /Dest form.
                let i = *index;
                (i >= 0).then(|| (i as usize) + 1)
            }
            _ => None,
        }
    }
}

/// Decode a PDF text string: UTF-16BE with a BOM, else the doc encoding
/// (PDFDocEncoding is Latin-1 for the printable range).
fn decode_text_string(raw: &[u8]) -> String {
    if raw.starts_with(&[0xFE, 0xFF]) {
        let units: Vec<u16> = raw[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if raw.starts_with(&[0xFF, 0xFE]) {
        let units: Vec<u16> = raw[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    String::from_utf8_lossy(raw).into_owned()
}

/// Rebuild the cross-reference table by scanning the body for `N G obj`.
///
/// The spec's recovery rule: the xref table is the fast path, but a stale
/// one (an incremental update that moved objects) is worse than a scan. The
/// scan is exact — an object's `obj` keyword is unambiguous — and costs one
/// pass over the file, which for a shelf of books is the honest price of a
/// correct table.
fn rebuild_xref(data: &[u8]) -> Result<HashMap<u32, Obj>, PdfError> {
    // Walk the body for "<id> <gen> obj". The keyword must stand alone —
    // preceded by whitespace and followed by whitespace, a delimiter or the
    // end of the file — or "obj" inside a stream's compressed bytes would be
    // read as an object header.
    //
    // The scan runs on the raw bytes, and the offsets it records are raw
    // byte offsets: a `from_utf8_lossy` over the whole file replaces invalid
    // sequences and moves every offset after them, which is how the first
    // cut of this indexed 22 MB into a 16 MB file.
    let mut offsets: HashMap<u32, usize> = HashMap::new();
    let mut i = 0usize;
    while i + 3 <= data.len() {
        if &data[i..i + 3] != b"obj" {
            i += 1;
            continue;
        }
        let before_ok = i > 0 && matches!(data[i - 1], b'\n' | b'\r' | b' ' | b'\t');
        let after_ok = match data.get(i + 3).copied() {
            None => true,
            Some(c) => matches!(c, b'\n' | b'\r' | b' ' | b'\t' | b'<' | b'[' | b'(' | b'/'),
        };
        if before_ok && after_ok {
            if let Some((id, start)) = read_object_header(data, i) {
                // The first occurrence wins: an incremental update appends
                // a rewritten object, and the xref points at the original.
                offsets.entry(id).or_insert(start);
            }
        }
        i += 3;
    }
    if offsets.is_empty() {
        return Err(PdfError::NoXref);
    }
    // Each object is parsed out of its own byte range — never out of the
    // whole file. A parser handed the whole file cannot know where an object
    // ends, so a stream with an unresolvable `/Length` (an indirect
    // reference, which is the common case) would read to the end of the
    // document and then keep looking. Bounding the slice to the *next*
    // object's start makes that impossible, and costs one sort.
    let mut starts: Vec<(u32, usize)> = offsets.into_iter().collect();
    starts.sort_by_key(|(_, offset)| *offset);
    let mut objects: HashMap<u32, Obj> = HashMap::with_capacity(starts.len());
    for (index, (id, offset)) in starts.iter().enumerate() {
        let end = starts
            .get(index + 1)
            .map(|(_, next)| *next)
            .unwrap_or(data.len());
        let slice = &data[*offset..end];
        if std::env::var_os("SN_PDF_DEBUG").is_some() {
            eprintln!("  obj {id} at {offset}..{end} ({} bytes)", slice.len());
        }
        if let Some(obj) = parse_object_at(slice) {
            objects.insert(*id, obj);
        }
    }
    // Streams whose `/Length` was an indirect reference were cut to nothing
    // (the number lives in an object the table had not reached). Every slice
    // is still in hand, so re-cut them now that the table is whole: find the
    // stream's data start in the slice, read the Length object, slice that
    // many bytes.
    for (id, offset) in starts.iter() {
        let end = starts
            .get(index_of(offset, &starts))
            .map(|(_, next)| *next)
            .unwrap_or(data.len());
        let length = match objects.get(id) {
            Some(Obj::Stream { dict, data }) if data.is_empty() => match dict.get("Length") {
                Some(Obj::Ref(r)) => match objects.get(r) {
                    Some(Obj::Int(n)) if *n > 0 => Some(*n as usize),
                    _ => None,
                },
                _ => None,
            },
            _ => continue,
        };
        let Some(length) = length else { continue };
        let slice = &data[*offset..end];
        let Some(rel) = find_stream_data(slice) else { continue };
        let start = rel + offset;
        let stop = (start + length).min(end);
        if stop > start {
            if let Some(Obj::Stream { data, .. }) = objects.get_mut(id) {
                *data = data[start..stop].to_vec();
            }
        }
    }
    // The trailer: the last `trailer` keyword's dictionary. Object 0 holds
    // it so `catalog()` and `trailer()` find it through the same map.
    if let Some(pos) = data.windows(7).rposition(|w| w == b"trailer") {
        if let Some(dict) = parse_dict_at(data, pos + 7) {
            objects.insert(0, Obj::Dict(dict));
        }
    } else {
        if std::env::var("SN_PDF_TRACE").map(|v| !v.is_empty()).unwrap_or(false) {
            eprintln!("TRACE: no trailer keyword; hunting the xref stream");
        }
        // No `trailer` keyword: a PDF 1.5+ cross-reference *stream* file.
        // The trailer lives in the xref stream's own dictionary (`/Root`,
        // `/Info`, `/Encrypt` are keys there), and that stream is the LAST
        // object in the file — so parse its header directly rather than
        // trusting any particular object id.
        if let Some(dict) = last_xref_stream_dict(data) {
            objects.insert(0, Obj::Dict(dict));
        }
        if std::env::var("SN_PDF_TRACE").map(|v| !v.is_empty()).unwrap_or(false) {
            eprintln!("TRACE: xref-stream trailer done");
        }
    }
    // Object streams: PDF 1.5+ packs ordinary objects into /ObjStm
    // containers. The page tree, the info dictionary and the outline of a
    // modern export usually live in one, so a shelf over such a file would
    // otherwise show zero pages. Each stream's /N header pairs and /First
    // offset are parsed and its members inserted — unless the id was already
    // present at top level, which wins (an incremental update overrides).
    let mut expanded: Vec<u32> = Vec::new();
    let ids: Vec<u32> = objects.keys().copied().collect();
    if std::env::var("SN_PDF_TRACE").map(|v| !v.is_empty()).unwrap_or(false) {
        eprintln!("TRACE: objstm expansion over {} streams", ids.len());
    }
    for id in ids {
        let Some(Obj::Stream { dict, data }) = objects.get(&id) else {
            continue;
        };
        let subtype = dict.get("Type").and_then(|o| o.as_name());
        if subtype != Some("ObjStm") {
            continue;
        }
        let count = match dict.get("N") {
            Some(Obj::Int(n)) => (*n).max(0) as usize,
            _ => 0,
        };
        let first = match dict.get("First") {
            Some(Obj::Int(f)) => (*f).max(0) as usize,
            _ => 0,
        };
        if std::env::var("SN_PDF_TRACE").map(|v| !v.is_empty()).unwrap_or(false) {
            eprintln!("TRACE: inflating ObjStm {id} ({} bytes)", data.len());
        }
        let pairs = inflate(data);
        let Some(pairs) = pairs else { continue };
        // The header is `count` pairs of (member id, relative offset), as
        // ASCII integers, starting at the stream's byte 0.
        let mut numbers: Vec<u32> = Vec::new();
        let mut p = 0usize;
        let header_end = first.min(pairs.len());
        while numbers.len() < count * 2 && p < header_end {
            while p < header_end && (pairs[p] as char).is_ascii_whitespace() {
                p += 1;
            }
            let start = p;
            while p < header_end && (pairs[p] as char).is_ascii_digit() {
                p += 1;
            }
            if start == p {
                break;
            }
            match std::str::from_utf8(&pairs[start..p])
                .ok()
                .and_then(|s| s.parse::<u32>().ok())
            {
                Some(n) => numbers.push(n),
                None => break,
            }
        }
        for pair in numbers.chunks(2) {
            let (member, rel) = (pair[0] as usize, pair[1] as usize);
            let start = first + rel;
            if start >= pairs.len() {
                continue;
            }
            let end = (start + pairs.len()).min(pairs.len());
            let slice = &pairs[start..end];
            let mut p2 = Parser::new_bytes(slice);
            p2.skip_whitespace();
            if let Some(obj) = p2.parse_object() {
                objects.entry(member as u32).or_insert(obj);
                expanded.push(member as u32);
            }
        }
    }
    let _ = expanded;
    if std::env::var("SN_PDF_TRACE").map(|v| !v.is_empty()).unwrap_or(false) {
        eprintln!("TRACE: rebuild done");
    }
    Ok(objects)
}

/// The last `/Type /XRef` stream's dictionary: a PDF 1.5+ file's trailer.
fn last_xref_stream_dict(data: &[u8]) -> Option<HashMap<String, Obj>> {
    let mut last: Option<usize> = None; // header offset of the last /Type /XRef object
    let mut i = 0usize;
    while i + 3 <= data.len() {
        if &data[i..i + 3] != b"obj" {
            i += 1;
            continue;
        }
        let before_ok = i > 0 && matches!(data[i - 1], b'\n' | b'\r' | b' ' | b'\t');
        let after_ok = match data.get(i + 3).copied() {
            None => true,
            Some(c) => matches!(c, b'\n' | b'\r' | b' ' | b'\t' | b'<' | b'[' | b'(' | b'/'),
        };
        if before_ok && after_ok {
            // Peek ahead at the dictionary: `/Type /XRef` decides. Both
            // spellings occur — with and without the separating space.
            let window_end = (i + 3 + 400).min(data.len());
            if let Some(dict_start) = data[i + 3..window_end]
                .windows(2)
                .position(|w| w == b"<<")
                .map(|p| i + 3 + p)
            {
                let window = &data[dict_start..window_end];
                let is_xref = window.windows(11).any(|w| w == b"/Type /XRef")
                    || window.windows(10).any(|w| w == b"/Type/XRef");
                if is_xref {
                    if let Some((_id, header)) = read_object_header(data, i) {
                        last = Some(header);
                    }
                }
            }
        }
        i += 3;
    }
    let header = last?;
    let text = String::from_utf8_lossy(&data[header..(header + 600).min(data.len())]);
    let mut p = Parser::new_bytes(text.as_bytes());
    p.skip_whitespace();
    p.skip_token();
    p.skip_token();
    p.skip_whitespace();
    if !p.eat_keyword("obj") {
        return None;
    }
    p.skip_whitespace();
    match p.parse_object() {
        Some(Obj::Dict(dict)) => Some(dict),
        Some(Obj::Stream { dict, .. }) => Some(dict),
        _ => None,
    }
}

/// Where a slice's stream DATA begins: the `stream` keyword after its
/// dictionary, plus the EOL that ends the keyword's line.
fn find_stream_data(slice: &[u8]) -> Option<usize> {
    let mut i = 0usize;
    // Skip the header and dictionary: find `stream` as a standalone word.
    while i + 6 <= slice.len() {
        if &slice[i..i + 6] == b"stream" {
            let before_ok = i == 0 || matches!(slice[i - 1], b'\n' | b'\r' | b' ' | b'\t');
            let after = slice.get(i + 6).copied();
            let after_ok = matches!(after, Some(b'\n') | Some(b'\r') | Some(b' ') | None);
            if before_ok && after_ok {
                let mut j = i + 6;
                if slice.get(j) == Some(&b'\r') {
                    j += 1;
                }
                if slice.get(j) == Some(&b'\n') {
                    j += 1;
                } else if slice.get(j).is_some_and(|c| c.is_ascii_whitespace()) {
                    j += 1;
                }
                return Some(j);
            }
        }
        i += 1;
    }
    None
}

/// The index of the entry whose offset equals `needle` (offsets are unique).
fn index_of(needle: &usize, starts: &[(u32, usize)]) -> usize {
    starts
        .iter()
        .position(|(_, o)| o == needle)
        .unwrap_or(usize::MAX)
}

/// Inflate a stream's bytes. FlateDecode is the workhorse: object streams,
/// cross-reference streams and content streams are all Flate-compressed in
/// every modern export.
fn inflate(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::ZlibDecoder::new(data).read_to_end(&mut out).ok()?;
    Some(out)
}

/// Decode LZW (PDF 1.1-era producers, word processors mostly): codes of 9–12
/// bits packed MSB-first, 256 clears the table, 257 ends the stream, and the
/// code width grows one early by default (Adobe's `EarlyChange 1`).
fn lzw_decode(data: &[u8], early_change: bool) -> Option<Vec<u8>> {
    fn bits(data: &[u8], at: usize, width: usize) -> Option<u32> {
        let mut v = 0u32;
        for i in 0..width {
            let p = at + i;
            let byte = *data.get(p / 8)?;
            v = (v << 1) | ((byte >> (7 - p % 8)) & 1) as u32;
        }
        Some(v)
    }

    let mut dict: Vec<Vec<u8>> = (0..256).map(|i| vec![i as u8]).collect();
    dict.push(Vec::new()); // 256: clear
    dict.push(Vec::new()); // 257: EOD
    let mut next = 258usize;
    let mut width = 9usize;
    let mut out = Vec::new();
    let mut prev: Option<Vec<u8>> = None;
    let mut at = 0usize;
    let total = data.len() * 8;
    while at + width <= total {
        let code = bits(data, at, width)? as usize;
        at += width;
        match code {
            256 => {
                dict.truncate(258);
                next = 258;
                width = 9;
                prev = None;
                continue;
            }
            257 => break,
            _ => {}
        }
        // The table lags the encoder by one entry: a code one past the last
        // written entry names `prev + prev[0]`, the encoder's lookahead.
        let entry: Vec<u8> = if code < dict.len() {
            dict[code].clone()
        } else if code == next {
            match prev.as_ref() {
                Some(p) if !p.is_empty() => {
                    let mut e = p.clone();
                    e.push(p[0]);
                    e
                }
                _ => break, // nothing established yet: the stream is corrupt
            }
        } else {
            break; // past the lookahead: corrupt from here on
        };
        out.extend_from_slice(&entry);
        if let Some(p) = prev.take() {
            if next < 4096 {
                dict.push(p.into_iter().chain(entry.iter().take(1).copied()).collect());
                next += 1;
                if width < 12 && next + usize::from(early_change) >= 1 << width {
                    width += 1;
                }
            }
        }
        prev = Some(entry);
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Decode ASCIIHexDecode: hex digits to a closing `>`, whitespace ignored,
/// an odd trailing digit padded with a zero high nibble.
fn asciihex_decode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut hi: Option<u8> = None;
    for &b in data {
        match b {
            b'>' => break,
            b'0'..=b'9' | b'a'..=b'f' | b'A'..=b'F' => {
                let v = match b {
                    b'0'..=b'9' => b - b'0',
                    b'a'..=b'f' => b - b'a' + 10,
                    _ => b - b'A' + 10,
                };
                match hi.take() {
                    None => hi = Some(v),
                    Some(h) => out.push(h * 16 + v),
                }
            }
            _ => {} // whitespace between digits
        }
    }
    out
}

/// Decode ASCII85Decode (`A85`): base-85 groups of five digits to four
/// bytes, `z` for a zero group, ending at `~>`. Whitespace between digits.
fn ascii85_decode(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut group = [0u32; 5];
    let mut n = 0usize;
    for &b in data {
        match b {
            b' ' | b'\t' | b'\n' | b'\r' | 0x0C | 0x00 => {}
            b'z' if n == 0 => out.extend_from_slice(&[0, 0, 0, 0]),
            b'~' => break, // the `>` of `~>` is trailing decoration
            b'!'..=b'u' => {
                group[n] = (b - b'!') as u32;
                n += 1;
                if n == 5 {
                    let mut v = 0u64;
                    for &d in &group {
                        v = v * 85 + d as u64;
                    }
                    if v > 0xFFFF_FFFF {
                        return None; // a group past 2^32: corrupt
                    }
                    out.extend_from_slice(&(v as u32).to_be_bytes());
                    n = 0;
                }
            }
            _ => return None, // a digit outside base 85: corrupt
        }
    }
    if n > 0 {
        // A partial final group of k bytes arrived as k+1 digits: pad the
        // digits AFTER them with `u` (84), decode, keep the first k bytes.
        for d in group.iter_mut().skip(n) {
            *d = 84;
        }
        let mut v = 0u64;
        for &d in &group {
            v = v * 85 + d as u64;
        }
        if v > 0xFFFF_FFFF {
            return None;
        }
        out.extend_from_slice(&(v as u32).to_be_bytes()[..n - 1]);
    }
    Some(out)
}

/// Decode RunLengthDecode: a length byte runs the next bytes — 0–127 copies
/// the following n+1 bytes literally, 129–254 repeats the following byte
/// 257-n times, 255 ends the stream.
fn runlength_decode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < data.len() {
        let n = data[i];
        i += 1;
        match n {
            0..=127 => {
                let end = (i + n as usize + 1).min(data.len());
                out.extend_from_slice(&data[i..end]);
                i = end;
            }
            129..=254 => {
                let Some(&b) = data.get(i) else { break };
                i += 1;
                out.extend(std::iter::repeat(b).take(257 - n as usize));
            }
            128 => break, // undefined per spec, treated as EOD
            255 => break,
        }
    }
    out
}

/// Undo a PNG predictor (PDF 1.5+ xref streams): each row is `1 + columns`
/// bytes, the first naming the filter that produced it. Only the filters
/// xref streams use are implemented (Sub, Up, Average, Paeth); None is an
/// identity row.
fn unpredict_png(data: &[u8], columns: usize) -> Vec<u8> {
    let row_len = columns + 1;
    let rows = data.len() / row_len;
    let mut out = vec![0u8; rows * columns];
    for row in 0..rows {
        let filter = data[row * row_len];
        let src = &data[row * row_len + 1..row * row_len + 1 + columns];
        for col in 0..columns {
            let x = src[col] as i32;
            let left = if col > 0 { out[row * columns + col - 1] as i32 } else { 0 };
            let up = if row > 0 { out[(row - 1) * columns + col] as i32 } else { 0 };
            let upleft = if row > 0 && col > 0 { out[(row - 1) * columns + col - 1] as i32 } else { 0 };
            let v = match filter {
                1 => x + left,
                2 => x + up,
                3 => x + (left + up) / 2,
                4 => {
                    let p = left + up - upleft;
                    let (pl, pu, pul) = (p - left, p - up, p - upleft);
                    let pa = pl.abs(); let pb = pu.abs(); let pc = pul.abs();
                    let pred = if pa <= pb && pa <= pc { left } else if pb <= pc { up } else { upleft };
                    x + pred
                }
                _ => x,
            };
            out[row * columns + col] = (v & 0xFF) as u8;
        }
    }
    out
}

/// Read the `<id> <gen>` in front of an `obj` keyword at `keyword`, and
/// return the id with the byte offset the header starts at.
///
/// The two tokens are read backwards, one whitespace-delimited word at a
/// time, over the raw bytes: the id and generation are ASCII digits and the
/// header is on one line, so this is exact and does not need the whole file
/// to be valid UTF-8.
fn read_object_header(data: &[u8], keyword: usize) -> Option<(u32, usize)> {
    // Walk back over the generation's digits, then the whitespace, then the
    // id's digits.
    let mut end = keyword;
    while end > 0 && matches!(data[end - 1], b' ' | b'\t') {
        end -= 1;
    }
    let gen_end = end;
    while end > 0 && data[end - 1].is_ascii_digit() {
        end -= 1;
    }
    let gen_start = end;
    while end > 0 && matches!(data[end - 1], b' ' | b'\t') {
        end -= 1;
    }
    let id_end = end;
    while end > 0 && data[end - 1].is_ascii_digit() {
        end -= 1;
    }
    let id_start = end;
    if gen_start == gen_end || id_start == id_end {
        return None;
    }
    let id = std::str::from_utf8(&data[id_start..id_end])
        .ok()?
        .parse::<u32>()
        .ok()?;
    let gen = std::str::from_utf8(&data[gen_start..gen_end])
        .ok()?
        .parse::<u32>()
        .ok()?;
    let _ = gen;
    Some((id, id_start))
}

/// Parse one object from a slice that begins at its `<id> <gen> obj`
/// header and ends at the next object's header (or the end of the file).
///
/// The bound is what makes a stream safe: a `/Length` that is an indirect
/// reference cannot be resolved while the table is still being built, so the
/// stream's bytes are cut by the slice's end, which is exactly where the
/// next object starts.
///
/// The slice is scanned as *bytes* with a byte-position parser, never a
/// `from_utf8_lossy` string: a JPEG stream's bytes are not text, and every
/// `U+FFFD` the lossy conversion inserts shifts the string's indices off
/// the file's — a `/Length 189636` would then cut the stream at the wrong
/// byte and a UTF-16BE literal's `\0M\0i` pairs would be torn apart.
fn parse_object_at(slice: &[u8]) -> Option<Obj> {
    let mut p = Parser::new_bytes(slice);
    p.skip_whitespace();
    // The id and generation are part of the header: skip them.
    p.skip_token();
    p.skip_token();
    p.skip_whitespace();
    if p.eat_keyword("obj") {
        p.skip_whitespace();
        return p.parse_object();
    }
    None
}


/// Parse a dictionary starting just after a `trailer` keyword. Bytes, for
/// the same reason `parse_object_at` is.
fn parse_dict_at(data: &[u8], offset: usize) -> Option<HashMap<String, Obj>> {
    let mut p = Parser::new_bytes(&data[offset..]);
    p.skip_whitespace();
    p.parse_dict()
}

/// The PDF object parser: names, numbers, strings, arrays, dictionaries,
/// references and streams. It runs on raw bytes: PDF strings and streams
/// are binary, and a text view of the file would corrupt both and shift
/// every offset after the first non-UTF-8 byte.
struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new_bytes(src: &'a [u8]) -> Self {
        Self { src, pos: 0 }
    }

    fn eof(&self) -> bool {
        self.pos >= self.src.len()
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    /// True when the keyword starts at the cursor.
    fn starts_with(&self, kw: &str) -> bool {
        self.src[self.pos.min(self.src.len())..].starts_with(kw.as_bytes())
    }

    fn skip_whitespace(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_ascii_whitespace() || c == b'%' {
                if c == b'%' {
                    // A comment runs to the end of the line.
                    while let Some(c) = self.peek() {
                        if c == b'\n' {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    /// Advance past one whitespace-delimited token.
    fn skip_token(&mut self) {
        self.skip_whitespace();
        while let Some(c) = self.peek() {
            if c.is_ascii_whitespace() || c == b'<' || c == b'[' || c == b'/' {
                break;
            }
            self.pos += 1;
        }
    }

    fn eat_keyword(&mut self, kw: &str) -> bool {
        self.skip_whitespace();
        if self.starts_with(kw) {
            self.pos += kw.len();
            true
        } else {
            false
        }
    }

    fn parse_object(&mut self) -> Option<Obj> {
        self.skip_whitespace();
        let c = self.peek()?;
        match c {
            b'/' => Some(Obj::Name(self.parse_name())),
            b'(' => Some(Obj::Str(self.parse_literal_string())),
            // A hex string `<FEFF0053…>`: the other spelling of a string.
            // UTF-16BE titles are usually written this way, and a missing
            // arm here did not merely drop the title — parse_object returned
            // None, which truncated the enclosing dict or array, and the
            // trailer's `/ID [<…> <…>]` then left parse_dict's recovery
            // loop sitting on a `<` it could not pass, spinning forever.
            b'<' if self.peek_at(1) != Some(b'<') => {
                Some(Obj::Str(self.parse_hex_string()))
            }
            b'<' if self.peek_at(1) == Some(b'<') => {
                let dict = self.parse_dict()?;
                // A stream follows its dictionary when `stream` is next.
                self.skip_whitespace();
                if self.starts_with("stream") {
                    let data = self.parse_stream_data(&dict);
                    Some(Obj::Stream { dict, data })
                } else {
                    Some(Obj::Dict(dict))
                }
            }
            b'[' => Some(Obj::Array(self.parse_array())),
            b't' if self.starts_with("true") => {
                self.pos += 4;
                Some(Obj::Bool(true))
            }
            b'f' if self.starts_with("false") => {
                self.pos += 5;
                Some(Obj::Bool(false))
            }
            b'n' if self.starts_with("null") => {
                self.pos += 4;
                Some(Obj::Null)
            }
            b'0'..=b'9' | b'+' | b'-' | b'.' => self.parse_number_or_ref(),
            _ => None,
        }
    }

    fn peek_at(&self, offset: usize) -> Option<u8> {
        self.src.get(self.pos + offset).copied()
    }

    /// A number, or `int int R` which is an indirect reference.
    fn parse_number_or_ref(&mut self) -> Option<Obj> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || matches!(c, b'+' | b'-' | b'.') {
                self.pos += 1;
            } else {
                break;
            }
        }
        let text = std::str::from_utf8(&self.src[start..self.pos]).unwrap_or("0");
        // The reference probe (`N N R`) must be *speculative*: `[0 0 612 792]`
        // reads "0", then a second number, and only the `R` after it decides.
        // Without saving the cursor the probe consumed "0 0" of the array as
        // a half-parsed reference and dropped the other two numbers.
        let save = self.pos;
        self.skip_whitespace();
        if self.peek().is_some_and(|c| c.is_ascii_digit()) {
            while let Some(c) = self.peek() {
                if c.is_ascii_digit() {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            self.skip_whitespace();
            if self.peek() == Some(b'R') {
                self.pos += 1;
                let id = text.parse::<u32>().ok()?;
                return Some(Obj::Ref(id));
            }
        }
        self.pos = save;
        if text.contains('.') {
            text.parse::<f64>().ok().map(Obj::Real)
        } else {
            text.parse::<i64>().ok().map(Obj::Int)
        }
    }

    fn parse_name(&mut self) -> String {
        self.pos += 1; // the '/'
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_whitespace() || matches!(c, b'/' | b'<' | b'[' | b'(' | b')' | b'>' | b']' | b'{' | b'}') {
                break;
            }
            // A name is ASCII by spec; a stray high byte is kept as its
            // Latin-1 glyph rather than silently dropped.
            out.push(c as char);
            self.pos += 1;
        }
        out
    }

    fn parse_literal_string(&mut self) -> Vec<u8> {
        self.pos += 1; // the '('
        let mut out = Vec::new();
        let mut depth = 1;
        while let Some(c) = self.peek() {
            self.pos += 1;
            match c {
                b'(' => {
                    depth += 1;
                    out.push(c);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    out.push(c);
                }
                b'\\' => {
                    // The escape rules that matter: a balanced paren, a
                    // backslash, an octal byte, and a line continuation.
                    match self.peek() {
                        Some(b'n') => {
                            out.push(b'\n');
                            self.pos += 1;
                        }
                        Some(b'r') => {
                            out.push(b'\r');
                            self.pos += 1;
                        }
                        Some(b't') => {
                            out.push(b'\t');
                            self.pos += 1;
                        }
                        Some(b'0'..=b'7') => {
                            let mut value = 0u32;
                            let mut digits = 0;
                            while digits < 3 {
                                if let Some(d @ b'0'..=b'7') = self.peek() {
                                    value = value * 8 + (d - b'0') as u32;
                                    self.pos += 1;
                                    digits += 1;
                                } else {
                                    break;
                                }
                            }
                            out.push(value as u8);
                        }
                        Some(other) => {
                            out.push(other);
                            self.pos += 1;
                        }
                        None => break,
                    }
                }
                other => out.push(other),
            }
        }
        out
    }

    /// The bytes of a hex string `<48 65 6C>`, whitespace ignored. An odd
    /// trailing digit is followed by an implicit 0, per the spec.
    fn parse_hex_string(&mut self) -> Vec<u8> {
        self.pos += 1; // the '<'
        let mut digits: u8 = 0;
        let mut hi: u8 = 0;
        let mut out = Vec::new();
        while let Some(c) = self.peek() {
            if c == b'>' {
                self.pos += 1;
                break;
            }
            self.pos += 1;
            if c.is_ascii_whitespace() {
                continue;
            }
            let v = match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                b'A'..=b'F' => c - b'A' + 10,
                _ => continue,
            };
            if digits == 0 {
                hi = v;
                digits = 1;
            } else {
                out.push(hi * 16 + v);
                digits = 0;
            }
        }
        if digits == 1 {
            out.push(hi * 16);
        }
        out
    }

    fn parse_array(&mut self) -> Vec<Obj> {
        self.pos += 1; // the '['
        let mut out = Vec::new();
        loop {
            self.skip_whitespace();
            if self.peek() == Some(b']') {
                self.pos += 1;
                break;
            }
            if self.eof() {
                break;
            }
            match self.parse_object() {
                Some(o) => out.push(o),
                None => break,
            }
        }
        out
    }

    fn parse_dict(&mut self) -> Option<HashMap<String, Obj>> {
        self.skip_whitespace();
        if self.peek() != Some(b'<') || self.peek_at(1) != Some(b'<') {
            return None;
        }
        self.pos += 2;
        let mut out = HashMap::new();
        loop {
            self.skip_whitespace();
            if self.peek() == Some(b'>') && self.peek_at(1) == Some(b'>') {
                self.pos += 2;
                break;
            }
            if self.eof() {
                break;
            }
            if self.peek() != Some(b'/') {
                // A malformed key: skip a token rather than losing the rest.
                // skip_token can refuse to advance (its break set starts with
                // '<' and '['), so consume one byte here or this loop spins
                // forever on a dictionary it cannot name a key for.
                let before = self.pos;
                self.skip_token();
                if self.pos == before {
                    if self.peek().is_some() {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
                continue;
            }
            let key = self.parse_name();
            match self.parse_object() {
                Some(value) => {
                    out.insert(key, value);
                }
                None => break,
            }
        }
        Some(out)
    }

    /// The bytes after `stream`, cut by `/Length`.
    fn parse_stream_data(&mut self, dict: &HashMap<String, Obj>) -> Vec<u8> {
        // `stream` then EOL, then Length bytes, then `endstream`.
        self.skip_whitespace();
        if self.starts_with("stream") {
            self.pos += 6;
        }
        if self.peek() == Some(b'\r') {
            self.pos += 1;
        }
        if self.peek() == Some(b'\n') {
            self.pos += 1;
        }
        let length = dict
            .get("Length")
            .and_then(|l| match l {
                Obj::Int(i) => Some(*i as usize),
                // An indirect `/Length` cannot be resolved while the table is
                // still being built. Falling back to 0 does not corrupt the
                // table — the next object's header bounds the slice either
                // way — and no consumer of stream *values* exists yet.
                _ => None,
            })
            .unwrap_or(0);
        let end = (self.pos + length).min(self.src.len());
        let data = self.src[self.pos..end].to_vec();
        self.pos = end;
        data
    }
}

impl Obj {
    /// A real number, following a reference.
    pub fn as_real(&self, doc: &Document) -> Option<f64> {
        match self {
            Obj::Real(f) => Some(*f),
            Obj::Int(i) => Some(*i as f64),
            Obj::Ref(id) => doc.objects.get(id).and_then(|o| o.as_real(doc)),
            _ => None,
        }
    }
}

/// Read a PDF's metadata from a path.
pub fn parse_file(path: &Path) -> Result<PdfInfo, PdfError> {
    let bytes = std::fs::read(path)?;
    parse(&bytes)
}


#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal one-page PDF with a classic xref table and trailer, built
    /// by hand: the fixture the recovery paths must never fail on.
    fn classic_pdf() -> Vec<u8> {
        let objects: Vec<String> = vec![
            "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".into(),
            "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".into(),
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n".into(),
            "4 0 obj\n<< /Title (Hello) /Author (World) >>\nendobj\n".into(),
        ];
        let mut body = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for o in &objects {
            offsets.push(body.len());
            body.push_str(o);
        }
        let xref_at = body.len();
        body.push_str("xref\n0 5\n");
        body.push_str("0000000000 65535 f \n");
        for off in &offsets {
            body.push_str(&format!("{off:010} 00000 n \n"));
        }
        body.push_str("trailer\n<< /Size 5 /Root 1 0 R /Info 4 0 R >>\n");
        body.push_str(&format!("startxref\n{xref_at}\n%%EOF\n"));
        body.into_bytes()
    }

    #[test]
    fn classic_trailer_answers_the_shelf() {
        let info = parse(&classic_pdf()).expect("parse");
        assert_eq!(info.page_count, 1);
        assert_eq!(info.title.as_deref(), Some("Hello"));
        assert_eq!(info.author.as_deref(), Some("World"));
        assert_eq!(info.version.as_deref(), Some("1.4"));
        assert_eq!(info.page_size, Some((612.0, 792.0)));
    }

    #[test]
    fn hex_string_titles_decode() {
        // UTF-16BE "Hi" as a hex string — the PowerPoint outline form that
        // used to fall through every arm and spin parse_dict forever.
        let mut pdf = classic_pdf();
        let joined = String::from_utf8(pdf).unwrap().replace(
            "/Title (Hello)",
            "/Title <FEFF00480069>",
        );
        pdf = joined.into_bytes();
        let info = parse(&pdf).expect("parse");
        assert_eq!(info.title.as_deref(), Some("Hi"));
    }

    #[test]
    fn utf16be_literal_titles_decode() {
        // \376\377 then UTF-16BE pairs inside a literal string.
        let mut title = vec![0xFE, 0xFF, 0x00, b'M'];
        let mut pdf = classic_pdf();
        let text = String::from_utf8(pdf).unwrap().replace(
            "(Hello)",
            &{
                let mut s = String::from("(");
                for b in &title {
                    // escape raw bytes as octal so they survive the literal
                    s.push_str(&format!("\\{:03o}", b));
                }
                s.push(')');
                s
            },
        );
        let _ = &mut title;
        pdf = text.into_bytes();
        let info = parse(&pdf).expect("parse");
        assert_eq!(info.title.as_deref(), Some("M"));
    }

    #[test]
    fn a_binary_stream_does_not_shift_the_table() {
        // A DCTDecode (JPEG) stream: its bytes are not UTF-8, and a parser
        // over a lossy *string* of them once mis-sliced every later offset.
        let jpeg: Vec<u8> = (0..600u32).map(|i| (i * 37 % 256) as u8).collect();
        let mut body = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for o in [
            "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_string(),
            "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_string(),
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 99 99] >>\nendobj\n".to_string(),
            format!("4 0 obj\n<< /Length {} /Filter /DCTDecode >>\nstream\n", jpeg.len()),
        ] {
            offsets.push(body.len());
            body.push_str(&o);
        }
        let stream_at = body.len();
        body.push_str("placeholder");
        let stream_len = body.len() - stream_at;
        // splice the real jpeg bytes in place of the placeholder
        let mut out = body[..stream_at].as_bytes().to_vec();
        out.extend_from_slice(&jpeg);
        out.extend_from_slice(b"\nendstream\nendobj\n");
        let xref_at = out.len();
        out.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
        for off in &offsets {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n").as_bytes());
        let _ = stream_len;
        let info = parse(&out).expect("parse");
        assert_eq!(info.page_count, 1);
    }

    #[test]
    fn the_outline_flattens_with_pages() {
        let mut body = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        let objects = [
            "1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Outlines 5 0 R >>\nendobj\n",
            "2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>\nendobj\n",
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>\nendobj\n",
            "4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>\nendobj\n",
            "5 0 obj\n<< /Type /Outlines /First 6 0 R /Last 7 0 R /Count 2 >>\nendobj\n",
            "6 0 obj\n<< /Title (Chapter 1) /Dest [3 0 R /XYZ 0 792 0] /Next 7 0 R >>\nendobj\n",
            "7 0 obj\n<< /Title (Chapter 2) /Dest [4 0 R /XYZ 0 792 0] /Prev 6 0 R >>\nendobj\n",
        ];
        for o in objects {
            offsets.push(body.len());
            body.push_str(o);
        }
        let xref_at = body.len();
        body.push_str("xref\n0 8\n0000000000 65535 f \n");
        for off in &offsets {
            body.push_str(&format!("{off:010} 00000 n \n"));
        }
        body.push_str("trailer\n<< /Size 8 /Root 1 0 R >>\nstartxref\n");
        body.push_str(&format!("{xref_at}\n%%EOF\n"));
        let info = parse(body.as_bytes()).expect("parse");
        assert_eq!(info.page_count, 2);
        assert_eq!(info.outline.len(), 2);
        assert_eq!(info.outline[0].title, "Chapter 1");
        assert_eq!(info.outline[0].page, Some(1));
        assert_eq!(info.outline[1].page, Some(2));
    }

    #[test]
    fn the_pages_links_carry_rects_and_targets() {
        // Two pages; page 1 carries the book's own contents row (a /GoTo
        // whose /D points at page 2), a web address, a highlight that is
        // not a link, and a link whose destination names a page this walk
        // cannot resolve — which is left out, not guessed.
        let mut body = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        let objects = [
            "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
            "2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>\nendobj\n",
            // The shifted MediaBox matters: the rects are relative to the
            // box's own corner, not the world's origin.
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [20 10 612 802]\n /Annots [5 0 R 6 0 R 7 0 R 8 0 R] >>\nendobj\n",
            "4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
            // The contents row: /Dest straight to page 2's ref.
            "5 0 obj\n<< /Subtype /Link /Rect [100 700 300 720] /Dest [4 0 R /XYZ 0 792 0] >>\nendobj\n",
            // The web's kind: an /S /URI action.
            "6 0 obj\n<< /Subtype /Link /Rect [100 650 300 670]\n /A << /S /URI /URI (https://example.org/read) >> >>\nendobj\n",
            // Not a link at all.
            "7 0 obj\n<< /Subtype /Square /Rect [0 0 50 50] >>\nendobj\n",
            // A link the resolver cannot land: /Dest names a /Dests tree
            // this walk does not build.
            "8 0 obj\n<< /Subtype /Link /Rect [10 10 90 30] /Dest (section1) >>\nendobj\n",
        ];
        for o in objects {
            offsets.push(body.len());
            body.push_str(o);
        }
        let xref_at = body.len();
        body.push_str("xref\n0 9\n0000000000 65535 f \n");
        for off in &offsets {
            body.push_str(&format!("{off:010} 00000 n \n"));
        }
        body.push_str("trailer\n<< /Size 9 /Root 1 0 R >>\nstartxref\n");
        body.push_str(&format!("{xref_at}\n%%EOF\n"));
        let doc = Document::parse(body.as_bytes()).expect("parse");
        let links = doc.page_links();
        // The square and the unresolvable named destination are left out.
        assert_eq!(links.len(), 2, "{links:?}");
        // The GoTo row: on page 1, aiming at page 2, its rect relative to
        // the MediaBox corner and y measured from the box's top — the box
        // tops at 802, so a rect y of [700 720] sits 82..102 from the top.
        assert_eq!(links[0].page, 1);
        assert_eq!(links[0].target, PageLinkTarget::Page(2));
        let (x0, y0, x1, y1) = links[0].rect;
        assert!((x0 - 80.0).abs() < 1e-9, "{x0}");
        assert!((y0 - 82.0).abs() < 1e-9, "{y0}");
        assert!((y1 - 102.0).abs() < 1e-9, "{y1}");
        // The URI row rides through untouched.
        assert_eq!(links[1].target, PageLinkTarget::Uri("https://example.org/read".into()));
    }

    #[test]
    fn a_link_rects_corners_may_come_in_either_order() {
        // [urx ury llx lly] is legal; the rect still normalises.
        let mut body = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        let objects = [
            "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
            "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [4 0 R] >>\nendobj\n",
            "4 0 obj\n<< /Subtype /Link /Rect [300 720 100 700] /A << /S /GoTo /D [3 0 R /XYZ null null null] >> >>\nendobj\n",
        ];
        for o in objects {
            offsets.push(body.len());
            body.push_str(o);
        }
        let xref_at = body.len();
        body.push_str("xref\n0 5\n0000000000 65535 f \n");
        for off in &offsets {
            body.push_str(&format!("{off:010} 00000 n \n"));
        }
        body.push_str("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n");
        body.push_str(&format!("{xref_at}\n%%EOF\n"));
        let doc = Document::parse(body.as_bytes()).expect("parse");
        let links = doc.page_links();
        assert_eq!(links.len(), 1);
        let (x0, y0, x1, y1) = links[0].rect;
        assert!((x0 - 100.0).abs() < 1e-9 && (x1 - 300.0).abs() < 1e-9, "{links:?}");
        assert!(y0 < y1, "y runs from the top: {links:?}");
        assert!((y0 - 72.0).abs() < 1e-9, "{links:?}");
        assert!(matches!(links[0].target, PageLinkTarget::Page(1)), "{links:?}");
    }

    #[test]
    fn encryption_is_detected_not_defeated() {
        let mut pdf = classic_pdf();
        let t = String::from_utf8(pdf).unwrap().replace(
            "/Root 1 0 R /Info 4 0 R",
            "/Root 1 0 R /Info 4 0 R /Encrypt 8 0 R",
        );
        pdf = t.into_bytes();
        let info = parse(&pdf).expect("a trailer with /Encrypt still parses");
        assert!(info.encrypted);
    }

    #[test]
    fn not_a_pdf_is_refused() {
        assert!(matches!(parse(b"plain text"), Err(PdfError::NotAPdf)));
    }

    // --- stream decoding ---------------------------------------------------

    /// A one-page PDF whose single stream object carries `dict` (the dict
    /// fragment, `/Filter …` etc.) and `data` as its raw bytes — the
    /// harness for the stream-decoding tests.
    fn stream_doc(dict: &str, data: &[u8]) -> Document {
        let mut body: Vec<u8> = b"%PDF-1.4\n".to_vec();
        let mut offsets: Vec<usize> = Vec::new();
        let head = [
            "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
            "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>\nendobj\n",
        ];
        for o in head {
            offsets.push(body.len());
            body.extend_from_slice(o.as_bytes());
        }
        offsets.push(body.len());
        body.extend_from_slice(format!("4 0 obj\n<< {} /Length {} >>\nstream\n", dict, data.len()).as_bytes());
        body.extend_from_slice(data);
        body.extend_from_slice(b"\nendstream\nendobj\n");
        let xref_at = body.len();
        body.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
        for off in &offsets {
            body.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        body.extend_from_slice(b"trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n");
        body.extend_from_slice(format!("{xref_at}\n%%EOF\n").as_bytes());
        Document::parse(&body).expect("stream fixture parses")
    }

    fn flate(data: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }

    /// Base-85 encode, `!`–`u` digits with a `~>` terminator — the encoder
    /// side of [`ascii85_decode`], written from the spec independently.
    fn ascii85(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for chunk in data.chunks(4) {
            let mut v = 0u64;
            for (i, &b) in chunk.iter().enumerate() {
                v += (b as u64) << (8 * (3 - i));
            }
            let mut digits = [0u32; 5];
            for d in digits.iter_mut().rev() {
                *d = (v % 85) as u32;
                v /= 85;
            }
            let take = if chunk.len() == 4 { 5 } else { chunk.len() + 1 };
            for &d in digits.iter().take(take) {
                out.push(b'!' + d as u8);
            }
        }
        out.extend_from_slice(b"~>");
        out
    }

    /// LZW encode with EarlyChange 1 — the encoder side of
    /// [`lzw_decode`], written from the spec independently.
    fn lzw_encode(data: &[u8]) -> Vec<u8> {
        use std::collections::HashMap;
        let mut dict: HashMap<Vec<u8>, u32> = (0..256).map(|i| (vec![i as u8], i as u32)).collect();
        let mut next = 258u32;
        let mut width = 9usize;
        let mut out = Vec::new();
        let mut acc = 0u64;
        let mut nbits = 0usize;
        fn emit(code: u32, width: usize, acc: &mut u64, nbits: &mut usize, out: &mut Vec<u8>) {
            *acc = (*acc << width) | code as u64;
            *nbits += width;
            while *nbits >= 8 {
                *nbits -= 8;
                out.push(((*acc >> *nbits) & 0xFF) as u8);
            }
        }
        let mut w: Vec<u8> = Vec::new();
        for &b in data {
            let mut wc = w.clone();
            wc.push(b);
            if dict.contains_key(&wc) {
                w = wc;
            } else {
                let code = dict[&w];
                emit(code, width, &mut acc, &mut nbits, &mut out);
                dict.insert(wc, next);
                next += 1;
                if width < 12 && next + 1 >= 1 << width {
                    width += 1;
                }
                w = vec![b];
            }
        }
        if !w.is_empty() {
            let code = dict[&w];
            emit(code, width, &mut acc, &mut nbits, &mut out);
        }
        emit(257, width, &mut acc, &mut nbits, &mut out);
        if nbits > 0 {
            out.push(((acc << (8 - nbits)) & 0xFF) as u8);
        }
        out
    }

    #[test]
    fn filter_array_form_decodes_flate() {
        // The form one real book uses for EVERY stream: `[/FlateDecode]`.
        let payload = b"BT /F1 12 Tf (decoded) Tj ET";
        let doc = stream_doc("/Filter [/FlateDecode]", &flate(payload));
        let obj = doc.get(4).expect("stream object");
        assert_eq!(doc.stream_data(obj).as_deref(), Some(&payload[..]));
    }

    #[test]
    fn filter_chain_ascii85_then_flate_decodes_in_order() {
        let payload = b"chained payload";
        let data = ascii85(&flate(payload));
        let doc = stream_doc("/Filter [/ASCII85Decode /FlateDecode]", &data);
        let obj = doc.get(4).expect("stream object");
        assert_eq!(doc.stream_data(obj).as_deref(), Some(&payload[..]));
    }

    #[test]
    fn asciihex_and_runlength_decode() {
        let hex: Vec<u8> = b"6173636969>"
            .iter()
            .copied()
            .collect();
        let doc = stream_doc("/Filter /ASCIIHexDecode", &hex);
        assert_eq!(
            doc.stream_data(doc.get(4).unwrap()).as_deref(),
            Some(b"ascii".as_slice())
        );
        // 0x02 copies the next 3 bytes literally; 0x01 the next 2; 0xFF ends.
        let rl = vec![2u8, b'a', b'b', b'c', 1, b'd', b'e', 255];
        let doc = stream_doc("/Filter /RunLengthDecode", &rl);
        assert_eq!(
            doc.stream_data(doc.get(4).unwrap()).as_deref(),
            Some(b"abcde".as_slice())
        );
    }

    #[test]
    fn lzw_roundtrips_and_grows_its_codes() {
        let payload = b"aaaaabbbbbcccccabcaaaa";
        let doc = stream_doc("/Filter /LZWDecode", &lzw_encode(payload));
        assert_eq!(
            doc.stream_data(doc.get(4).unwrap()).as_deref(),
            Some(&payload[..])
        );
        // The same through an array-form filter, as old writers write it.
        let doc = stream_doc("/Filter [/LZWDecode]", &lzw_encode(payload));
        assert_eq!(
            doc.stream_data(doc.get(4).unwrap()).as_deref(),
            Some(&payload[..])
        );
    }

    #[test]
    fn predictor_parms_read_from_the_array_form() {
        // Rows of 4 bytes, filter byte Up (2): rows 2 and 3 are deltas 0.
        let raw = vec![
            0u8, 10, 20, 30, 40, // row 1: literal
            2, 0, 0, 0, 0, // row 2: +0 each
            2, 1, 1, 1, 1, // row 3: +1 each
        ];
        let doc = stream_doc(
            "/Filter [/FlateDecode] /DecodeParms [<</Predictor 12 /Columns 4>>]",
            &flate(&raw),
        );
        assert_eq!(
            doc.stream_data(doc.get(4).unwrap()).as_deref(),
            Some(&[10u8, 20, 30, 40, 10, 20, 30, 40, 11, 21, 31, 41][..])
        );
    }

    #[test]
    fn an_unknown_terminal_filter_returns_raw() {
        let doc = stream_doc("/Filter /DCTDecode", b"\xff\xd8 jpeg bytes");
        assert_eq!(
            doc.stream_data(doc.get(4).unwrap()).as_deref(),
            Some(b"\xff\xd8 jpeg bytes".as_slice())
        );
    }
}
