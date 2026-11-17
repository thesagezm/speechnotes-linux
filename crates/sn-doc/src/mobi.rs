//! Mobipocket / Kindle reader — `.mobi`/`.azw`/`.azw3`/`.prc`/`.pdb`, one
//! parser: a self-contained Palm Database reader, the PalmDOC decompressor,
//! the Huff/CDIC decompressor ([`crate::huffcdic`]), and an HTML-to-chapters
//! pass.
//!
//! ## What the parse takes
//!
//! - **PDB header → record 0 MOBI header**: compression (1 = PalmDOC,
//!   2 = none, 17480 = HUFF/CDIC), the text record count, extra flags, the
//!   first image index, the codepage, and the encryption type — a non-zero
//!   one is a **DRM refusal**, said plainly rather than decoded as noise.
//! - **Hybrid files** (a MOBI6 part with a KF8 part behind a `BOUNDARY`
//!   record, named by EXTH 116): the KF8 part wins, and every record
//!   number its header declares is rebased onto where it starts.
//! - **Text records** (records `1..=count`), each stripped of its
//!   trailing-entry table first, then decompressed — PalmDOC across the
//!   *joined* records (a back-reference may cross a record boundary),
//!   Huff/CDIC record by record (its bitstream restarts at each boundary).
//! - **EXTH** metadata inside record 0: title (503), author (100),
//!   language (524), cover and thumbnail record offsets (201/202).
//! - **Images**: `<img recindex="N">` (MOBI6) and `<img
//!   src="kindle:embed:NNNN…">` (KF8) resolve into the resource records
//!   and become the book's `img/NNNNNN` pool, in first-use order. A
//!   reference nothing answers loses its tag.
//! - **Chapters**, by the first flow that applies: the book's own contents
//!   (CNCX index, cut at the publisher's byte offsets with snapping to tag
//!   boundaries), then — for KF8 rawML — the `<html>…</html>` flow
//!   boundaries, then headings (h1/h2 open chapters, deeper stay inside as
//!   prose), and finally chunking for a book with no structure at all.
//!
//! ## Provenance, and three things the iOS reference got wrong
//!
//! Ported in part from the sibling speechnotes-ios `MobiParser.swift`, the
//! user's own reference implementation — parsing only; this crate ends at
//! the [`BookDoc`] and the file stays on disk verbatim. Three reference
//! bugs surfaced against the ground truth while porting — calibre's
//! reader, which the reference itself cites, plus two real books on this
//! box (a 7.6 MB store `.azw3` and its calibre-produced `.mobi`). Each is
//! fixed here and pinned by a fixture:
//!
//! 1. **The reference's whole INDX/TAGX/CNCX chain was dead code**: it read
//!    its "tag count" out of the first byte of TAGX's *first-entry-offset*
//!    u32 (0x20 on every real book) and then walked 32 phantom tags. The
//!    real layout — TAGX as a first-entry offset, a control-byte count and
//!    4-byte `[tag, values, mask, eof]` entries; index entries as
//!    `[length byte][ident][control bytes][masked vwi values]`; CNCX
//!    records as bare vwi-length-prefixed strings — is what this module
//!    reads, and the index record number comes from the header field the
//!    format defines for it (0xF4), not a guess.
//! 2. **EXTH 202, not 203, is the thumbnail record offset**; 203 is the
//!    fake-cover flag — the reference read that 0/1 as a record number.
//! 3. **A standalone KF8 references images as `kindle:embed:NNNN`**, not
//!    `recindex` — every `<img>` in the reference `.azw3` is the embed
//!    form. Both spellings resolve here.
//!
//! Not supported, and said so rather than half-done: DRM-locked books and
//! KFX containers. Incremental INDX updates and ORDT character maps are
//! skipped — the plain index shape every book this box has seen carries.

use super::bookdoc::{xml_escape, BookChapter, BookDoc, BookDocError, BookImage, BookToc};
use super::huffcdic;
use std::collections::HashMap;

/// Ceiling on chapters from one book. A book whose markup carries thousands
/// of headings must not build an array the reader then renders row by row.
const MAX_CHAPTERS: usize = 4_000;

/// The lane's bound on one chapter's blocks — a book with no structure at
/// all chunks the same way a heading-less text file does. (The reference
/// chunked at 12 000 characters instead; the item bound is what the other
/// parsers here do.)
const CHAPTER_BLOCK_LIMIT: usize = 150;

/// Ceiling on decompressed text — a book that expands without bound must
/// not take the import down with it.
const MAX_TEXT_BYTES: usize = 64 * 1024 * 1024;

/// The DRM refusal, verbatim what the import path shows the user.
const DRM_MESSAGE: &str =
    "This file is DRM-locked. Books bought from a store are encrypted and can't be read here.";

/// Parse a whole Mobipocket file. Pure and CPU-bound — callers run it off
/// the main thread (a 50 MB `.azw3` spends real time in here).
pub fn parse(data: &[u8]) -> Result<BookDoc, BookDocError> {
    let pdb = PalmDatabase::parse(data)?;
    // The hybrid preference: a MOBI6 part with a KF8 part behind a BOUNDARY
    // record reads the KF8 part, rebase and all. `base` is the record
    // number every header-declared number is added to.
    let (header, base) = pick_header(&pdb)?;
    if header.encryption != 0 {
        return Err(BookDocError::Unsupported(DRM_MESSAGE.to_string()));
    }
    let exth = exth_of(&pdb, base, &header);
    match header.compression {
        1 | 2 | 17480 => {}
        other => {
            return Err(BookDocError::Malformed(format!(
                "unknown compression {other}"
            )))
        }
    }

    let records = collect_text_records(&pdb, &header, base)?;
    let mut body: Vec<u8> = match header.compression {
        17480 => {
            let (huff, cdics) = collect_huff_cdic_records(&pdb, &header, base)?;
            let mut reader = huffcdic::Reader::new(huff, &cdics).map_err(|_| {
                BookDocError::Malformed("the book's Huff/CDIC tables are corrupt".to_string())
            })?;
            let mut out = Vec::with_capacity(header.text_length.min(MAX_TEXT_BYTES));
            for record in &records {
                out.extend_from_slice(&reader.unpack(record));
                if out.len() > MAX_TEXT_BYTES {
                    return Err(too_large());
                }
            }
            out
        }
        _ => {
            // The PalmDOC path concatenates the per-record payloads first —
            // a back-reference may reach across a record boundary. Huff/CDIC
            // is the branch above: its bitstream restarts per record.
            let mut joined = Vec::with_capacity(records.iter().map(|r| r.len()).sum());
            for record in &records {
                joined.extend_from_slice(record);
            }
            let out = decompress_palmdoc(&joined);
            if out.len() > MAX_TEXT_BYTES {
                return Err(too_large());
            }
            out
        }
    };
    // Some writers end the last record with a sentinel '#'; it is not
    // content.
    if body.last() == Some(&b'#') {
        body.pop();
    }
    let html = decode_text(&body, header.codepage);

    // The prepared markup first (the head/style bodies and pagebreaks go),
    // then the images: the chapter walk below sees the rewritten `<img
    // src>`, and the pool holds only what the walk will reference.
    let prepared = prepare(&html);
    let (display_html, images) = extract_images(&prepared, &pdb, &header, base);
    // The book's own table of contents, from its index records. Read BEFORE
    // the chapter paths so they can cut at the publisher's offsets; empty
    // for a book that carries no index.
    let toc = table_of_contents(&pdb, &header, base);
    let (list, book_toc) = chapters(&display_html, header.is_kf8, &toc);
    if list.is_empty() {
        return Err(BookDocError::Malformed(
            "no readable text in the Mobipocket book".to_string(),
        ));
    }
    let chapters = list
        .into_iter()
        .map(|(title, html)| BookChapter { title, html })
        .collect();

    let cover = cover_image(&pdb, &header, base, &exth).map(|(mime, data)| BookImage {
        name: "img/cover".to_string(),
        mime: mime.to_string(),
        data: data.to_vec(),
    });
    Ok(BookDoc {
        title: exth.title.clone().or_else(|| header.title.clone()),
        author: exth.author.clone(),
        language: exth.language.clone(),
        cover,
        chapters,
        toc: book_toc,
        images,
    })
}

/// True when `data` looks like a Mobipocket book — cheap enough for an
/// extension sniff, and it rejects a `.mobi` that is really a ZIP, which is
/// the most common "wrong file" report. The `.pdb` extension is shared
/// with every other Palm-era format, so the import path gates on this
/// before handing a `.pdb` over.
pub fn looks_like_mobi(data: &[u8]) -> bool {
    if data.len() <= 68 {
        return false;
    }
    &data[60..64] == b"BOOK" && (&data[64..68] == b"MOBI" || &data[64..68] == b"REAd")
}

fn too_large() -> BookDocError {
    BookDocError::TooLarge {
        k: MAX_TEXT_BYTES / 1024,
        m: MAX_TEXT_BYTES / (1024 * 1024),
    }
}

// ---------------------------------------------------------------------------
// The Palm Database
// ---------------------------------------------------------------------------

/// Minimal Palm Database reader: the header, the record-offset list, and
/// record access by index — v0, the only kind Mobipocket books use.
///
/// The record list has a documented quirk: when the "next record IDs" list
/// begins with a zero, the offsets list is padded to a 32-bit boundary
/// (four bytes per entry instead of eight). Both layouts are handled,
/// because the first draft read only the eight-byte form and every book
/// written by a 32-bit-era Palm tool decoded as offsets of zero.
struct PalmDatabase<'a> {
    archive: &'a [u8],
    /// Offsets into `archive`; a deleted record sits at 0 and reads as
    /// `None`.
    offsets: Vec<usize>,
}

impl<'a> PalmDatabase<'a> {
    fn parse(archive: &'a [u8]) -> Result<Self, BookDocError> {
        if archive.len() <= 86 {
            return Err(BookDocError::Malformed(
                "not a Mobipocket book — the file is too small for a Palm Database".to_string(),
            ));
        }
        // BOOKMOBI is every Kindle-era book; REAd is ancient PRC.
        if &archive[60..64] != b"BOOK"
            || (&archive[64..68] != b"MOBI" && &archive[64..68] != b"REAd")
        {
            return Err(BookDocError::Malformed(
                "not a Mobipocket book — the Palm Database is not of the book type".to_string(),
            ));
        }
        let count = u16(archive, 76) as usize;
        if count == 0 || count >= 200_000 {
            return Err(BookDocError::Malformed(format!(
                "record count {count} out of range"
            )));
        }
        // Two record-list layouts exist: 8 bytes per entry (offset, id,
        // attributes) and 4 bytes per entry (offset only). The spec's hint —
        // "a zero first entry in the next-IDs list means the list is
        // padded" — is NOT usable: a normal book's next-IDs list is ALL
        // ZEROS ("no next record"), so the hint is true for essentially
        // every real `.azw3` and the 8-byte list then reads as garbage.
        // Choosing by VALIDITY instead is what a real reader has to do:
        // read both and keep the one whose offsets are all inside the file
        // and non-decreasing.
        let narrow = Self::offsets(archive, 8, count);
        let padded = Self::offsets(archive, 4, count);
        let offsets = if Self::looks_valid(archive, &padded) && !Self::looks_valid(archive, &narrow) {
            padded
        } else {
            narrow
        };
        Ok(Self { archive, offsets })
    }

    /// Reads `count` offsets with the given entry stride. An entry that
    /// runs off the end yields a sentinel 0 that `looks_valid` rejects.
    fn offsets(archive: &[u8], entry_size: usize, count: usize) -> Vec<usize> {
        let mut result = Vec::with_capacity(count);
        let mut cursor = 78;
        for _ in 0..count {
            if cursor + 4 <= archive.len() {
                result.push(u32(archive, cursor) as usize);
            } else {
                result.push(0);
            }
            cursor += entry_size;
        }
        result
    }

    /// A layout is plausible when every offset is inside the file and the
    /// list runs forwards (records are stored in order).
    fn looks_valid(archive: &[u8], candidate: &[usize]) -> bool {
        if candidate.is_empty() {
            return false;
        }
        let mut previous = 0;
        for &offset in candidate {
            if offset < 78 || offset + 2 >= archive.len() || offset < previous {
                return false;
            }
            previous = offset;
        }
        true
    }

    fn record_count(&self) -> usize {
        self.offsets.len()
    }

    /// Record bytes at the entry's offset. A deleted record (offset 0)
    /// reads as `None` rather than a slice at a nonsense index. There is
    /// NO per-record checksum in the format — a draft that stripped two
    /// bytes shifted every real book's records and broke the header magic.
    fn record(&self, index: usize) -> Option<&'a [u8]> {
        let start = *self.offsets.get(index)?;
        if start == 0 || start >= self.archive.len() {
            return None;
        }
        let end = self
            .offsets
            .get(index + 1)
            .copied()
            .unwrap_or(self.archive.len());
        if end <= start || end > self.archive.len() {
            return None;
        }
        Some(&self.archive[start..end])
    }
}

// ---------------------------------------------------------------------------
// Record 0: the MOBI header and its EXTH
// ---------------------------------------------------------------------------

/// The header record's fields. Offsets are the MobileRead layout; the ones
/// past 0xE4 exist only on writers long enough to carry them.
struct MobiHeader {
    compression: u16,
    text_length: usize,
    text_record_count: usize,
    encryption: u16,
    codepage: u32,
    is_kf8: bool,
    /// extra flags: how many trailing-entry counts each text record ends
    /// with. The field only exists once the header is at least 0xF4 bytes
    /// long, and a header length of exactly 0xE4 is a known kindlegen bug
    /// where the field holds garbage.
    extra_flags: u16,
    /// The record number of the first image resource, local to this part of
    /// the file; 0 when the writer left it unset.
    first_image_index: usize,
    /// The record number of the NCX index — the book's own contents; 0
    /// when the writer carried none.
    ncx_index: usize,
    /// The header's own title, for writers that keep it outside the EXTH.
    title: Option<String>,
    /// Where the EXTH block sits inside the header record, when there is
    /// one.
    exth_range: Option<(usize, usize)>,
}

impl MobiHeader {
    fn parse(record: &[u8]) -> Result<Self, BookDocError> {
        if record.len() < 16 {
            return Err(BookDocError::Malformed("header record too short".to_string()));
        }
        let compression = u16(record, 0);
        let text_length = u32(record, 4) as usize;
        let text_record_count = u16(record, 8) as usize;
        let record_size = u16(record, 10) as usize;
        let encryption = u16(record, 12);
        // Ancient PRC files stop after the PalmDOC header (≤16 bytes): no
        // MOBI magic, no title, no EXTH. They still read.
        if record.len() <= 16 {
            return Ok(Self {
                compression,
                text_length,
                text_record_count,
                encryption,
                codepage: 1252,
                is_kf8: false,
                extra_flags: 0,
                first_image_index: 0,
                ncx_index: 0,
                title: None,
                exth_range: None,
            });
        }
        let magic = &record[16..20];
        if magic != b"MOBI" && magic != b"BOOK" && magic != b"TEXt" {
            return Err(BookDocError::Malformed(
                "not a Mobipocket book — the header record carries no MOBI header".to_string(),
            ));
        }
        if record_size == 0 || record_size > 64 * 1024 {
            return Err(BookDocError::Malformed(format!(
                "implausible record size {record_size}"
            )));
        }
        if text_record_count == 0 || text_record_count >= 100_000 {
            return Err(BookDocError::Malformed(format!(
                "implausible text record count {text_record_count}"
            )));
        }
        if record.len() < 40 {
            return Err(BookDocError::Malformed("truncated MOBI header".to_string()));
        }
        let header_length = u32(record, 20) as usize;
        let codepage = u32(record, 28);
        let is_kf8 = u32(record, 0x68) == 8;

        // Title: an offset/length pair into record 0, or zero on writers
        // that keep the real title in EXTH only.
        let title_offset = u32(record, 0x54) as usize;
        let title_length = u32(record, 0x58) as usize;
        let mut title = None;
        if title_offset > 0
            && title_length > 0
            && title_offset + title_length <= record.len()
            && title_offset + title_length < 0xFFFF_FFFF
        {
            let value = decode_text(&record[title_offset..title_offset + title_length], codepage)
                .trim()
                .to_string();
            if !value.is_empty() {
                title = Some(value);
            }
        }

        let extra_flags = if (0xE5..=0x1000).contains(&header_length) && record.len() >= 0xF4 {
            u16(record, 0xF2)
        } else {
            0
        };
        let first_image_index = plausible_record(u32(record, 0x6C), record.len() >= 0x70);
        let ncx_index = plausible_record(u32(record, 0xF4), record.len() >= 0xF8);

        // EXTH sits immediately after the fixed header when the flag word
        // at 0x80 has bit 6 set. Scanning for the "EXTH" magic as a
        // fallback catches writers that set that flag wrongly — common
        // enough to be worth the belt and braces.
        let mut exth_range = None;
        let exth_flag = if record.len() >= 0x84 { u32(record, 0x80) } else { 0 };
        let start = 16 + header_length;
        if exth_flag & 0x40 != 0
            && start + 12 <= record.len()
            && &record[start..start + 4] == b"EXTH"
        {
            exth_range = Some((start, record.len()));
        }
        if exth_range.is_none() {
            exth_range = find_exth(record);
        }

        Ok(Self {
            compression,
            text_length,
            text_record_count,
            encryption,
            codepage,
            is_kf8,
            extra_flags,
            first_image_index,
            ncx_index,
            title,
            exth_range,
        })
    }
}

/// A record-number header field, when the record is long enough to carry
/// it and the writer set it. 0 = unset.
fn plausible_record(value: u32, long_enough: bool) -> usize {
    if long_enough && value > 0 && value != 0xFFFF_FFFF && (value as usize) <= 100_000 {
        value as usize
    } else {
        0
    }
}

/// The EXTH block found by its magic, validated against its declared
/// length. `None` when there is no plausible one.
fn find_exth(record: &[u8]) -> Option<(usize, usize)> {
    let at = record
        .get(16..)?
        .windows(4)
        .position(|w| w == b"EXTH")
        .map(|at| at + 16)?;
    let count = u32(record, at + 8);
    let length = u32(record, at + 4) as usize;
    if count < 2_000 && length >= 12 && length <= record.len() - at {
        Some((at, at + length))
    } else {
        None
    }
}

/// The metadata records inside an EXTH block. The handful the shelf and
/// the cover path show are read; subjects, ASINs, UUIDs and update stamps
/// are skipped rather than decoded.
struct Exth {
    title: Option<String>,
    author: Option<String>,
    language: Option<String>,
    /// EXTH 201/202, decoded: the record offset (from the first image
    /// record) of the cover / its thumbnail. `-1` when the writer left the
    /// field unset (`0xFFFFFFFF`).
    cover_offset: i64,
    thumbnail_offset: i64,
    /// EXTH 116: the record number where a KF8 part begins in a hybrid
    /// file (its previous record is the `BOUNDARY` sentinel).
    kf8_boundary: i64,
}

impl Default for Exth {
    /// The unset sentinel is `-1`: a zero cover offset is a VALID one (the
    /// first image record), which a derived `Default` would confuse.
    fn default() -> Self {
        Self {
            title: None,
            author: None,
            language: None,
            cover_offset: -1,
            thumbnail_offset: -1,
            kf8_boundary: -1,
        }
    }
}

/// The EXTH block of the header the parse reads (record `base`).
fn exth_of(pdb: &PalmDatabase, base: usize, header: &MobiHeader) -> Exth {
    let blob = pdb
        .record(base)
        .and_then(|record| header.exth_range.and_then(|(s, e)| record.get(s..e)))
        .unwrap_or(&[]);
    parse_exth(blob)
}

fn parse_exth(blob: &[u8]) -> Exth {
    let mut exth = Exth::default();
    if blob.len() < 12 || &blob[0..4] != b"EXTH" {
        return exth;
    }
    let count = u32(blob, 8);
    let mut cursor = 12usize;
    for _ in 0..count {
        if cursor + 8 > blob.len() {
            break;
        }
        let kind = u32(blob, cursor);
        let length = u32(blob, cursor + 4) as usize;
        if length < 8 || cursor + length > blob.len() {
            break;
        }
        let payload = &blob[cursor + 8..cursor + length];
        let text = decode_text(payload, 65001)
            .trim_matches('\u{0}')
            .trim()
            .to_string();
        match kind {
            100 if !text.is_empty() && exth.author.is_none() => exth.author = Some(text),
            503 if !text.is_empty() && exth.title.is_none() => exth.title = Some(text),
            524 if !text.is_empty() && exth.language.is_none() => exth.language = Some(text),
            201 if payload.len() >= 4 => {
                let value = u32(payload, payload.len() - 4);
                if value != 0xFFFF_FFFF {
                    exth.cover_offset = value as i64;
                }
            }
            // 202 is the thumbnail record offset; 203 is the fake-cover
            // flag — the reference read the flag as an offset, which sent
            // cover lookups at a record number that is 0 or 1.
            202 if payload.len() >= 4 => {
                let value = u32(payload, payload.len() - 4);
                if value != 0xFFFF_FFFF {
                    exth.thumbnail_offset = value as i64;
                }
            }
            116 if payload.len() >= 4 => {
                exth.kf8_boundary = u32(payload, payload.len() - 4) as i64
            }
            _ => {}
        }
        cursor += length;
    }
    exth
}

/// The header the parse reads: record 0, unless the file is a hybrid whose
/// KF8 part (named by EXTH 116, behind a `BOUNDARY` record) supersedes it.
/// Returns the header and the record base its numbers are relative to.
fn pick_header(pdb: &PalmDatabase) -> Result<(MobiHeader, usize), BookDocError> {
    let record0 = pdb
        .record(0)
        .ok_or_else(|| BookDocError::Malformed("missing book header record".to_string()))?;
    let header = MobiHeader::parse(record0)?;
    let kf8_boundary = parse_exth(header.exth_range.and_then(|(s, e)| record0.get(s..e)).unwrap_or(&[]))
        .kf8_boundary;
    if kf8_boundary > 0 {
        let boundary = kf8_boundary as usize;
        if boundary < pdb.record_count()
            && pdb.record(boundary - 1).is_some_and(|r| r == b"BOUNDARY")
        {
            if let Some(kf8_record) = pdb.record(boundary) {
                if let Ok(kf8_header) = MobiHeader::parse(kf8_record) {
                    // The KF8 part is its own book: prefer it unless it is
                    // the DRM'd one — a DRM refusal must name the part the
                    // reader would actually open.
                    if kf8_header.encryption == 0 || header.encryption != 0 {
                        return Ok((kf8_header, boundary));
                    }
                }
            }
        }
    }
    Ok((header, 0))
}

// ---------------------------------------------------------------------------
// Text records: trailing entries, PalmDOC, Huff/CDIC
// ---------------------------------------------------------------------------

/// The text records, each stripped of its trailing-entry table first. Text
/// always starts at record 1: EXTH lives INSIDE record 0 in Mobipocket
/// books, there is no separate EXTH record.
///
/// Returns the STRIPPED records rather than a joined blob: PalmDOC
/// decompresses the concatenation (a back-reference may span records), but
/// Huff/CDIC decompresses RECORD BY RECORD (its bitstream restarts at each
/// record boundary), so the caller needs the per-record payloads.
fn collect_text_records<'p>(
    pdb: &'p PalmDatabase,
    header: &MobiHeader,
    base: usize,
) -> Result<Vec<&'p [u8]>, BookDocError> {
    let count = header.text_record_count;
    if pdb.record_count() < base + 1 + count {
        return Err(BookDocError::Malformed(format!(
            "header declares {count} text record(s) but the file has {}",
            pdb.record_count().saturating_sub(base)
        )));
    }
    let mut out = Vec::with_capacity(count);
    let mut total = 0usize;
    for index in 0..count {
        let record = pdb.record(base + 1 + index).ok_or_else(|| {
            BookDocError::Malformed(format!("text record {} of {count} is missing", index + 1))
        })?;
        let cut = trailing_entry_bytes(record, header.extra_flags);
        let payload = if cut < record.len() {
            &record[..record.len() - cut]
        } else {
            record
        };
        total += payload.len();
        if total > MAX_TEXT_BYTES {
            return Err(too_large());
        }
        out.push(payload);
    }
    Ok(out)
}

/// The HUFF record and its CDIC dictionary records, found by magic among
/// the resource records after the text — their position is not fixed.
/// Missing tables are an error, not a fallback: the alternative is
/// decoding the bitstream as noise.
fn collect_huff_cdic_records<'p>(
    pdb: &'p PalmDatabase,
    header: &MobiHeader,
    base: usize,
) -> Result<(&'p [u8], Vec<&'p [u8]>), BookDocError> {
    let mut huff: Option<&'p [u8]> = None;
    let mut cdics: Vec<&'p [u8]> = Vec::new();
    for index in (base + 1 + header.text_record_count)..pdb.record_count() {
        let Some(record) = pdb.record(index) else { continue };
        if record.len() < 16 {
            continue;
        }
        if record[..4] == *b"HUFF" {
            if huff.is_none() {
                huff = Some(record);
            }
        } else if record[..4] == *b"CDIC" {
            cdics.push(record);
        }
    }
    match (huff, cdics.is_empty()) {
        (Some(huff), false) => Ok((huff, cdics)),
        _ => Err(BookDocError::Malformed(
            "Huff/CDIC compression is declared but its tables are missing".to_string(),
        )),
    }
}

/// One trailing-entry varint VALUE: seven bits per byte, read BACKWARD
/// from `size`, top bit marks the last byte, capped at 28 bits. This is
/// calibre's `sizeof_trailing_entry` — the value, not a byte count, is
/// what the caller adds.
fn trailing_count(record: &[u8], size: usize) -> usize {
    let mut bit_position = 0u32;
    let mut result = 0usize;
    let mut cursor = size;
    while cursor > 0 && bit_position < 28 {
        let byte = record[cursor - 1];
        result |= ((byte & 0x7F) as usize) << bit_position;
        bit_position += 7;
        cursor -= 1;
        if byte & 0x80 != 0 {
            return result;
        }
    }
    result
}

/// How many trailing bytes `extra_flags` says a record ends with.
///
/// This is calibre's `sizeof_trailing_entries`: bits 15…1 of the flags
/// peel varint entries from the record's end, and bit 0 adds
/// `(byte & 0x3) + 1`. Every set bit does the same backward-varint read,
/// so the peel order does not change the total — it only decides which
/// varint belongs to which flag, and the flags carry no meaning beyond
/// "an entry is here".
fn trailing_entry_bytes(record: &[u8], extra_flags: u16) -> usize {
    if extra_flags == 0 || record.is_empty() {
        return 0;
    }
    let mut total = 0usize;
    let mut bit = 15;
    while bit >= 1 {
        if extra_flags & (1 << bit) != 0 {
            if record.len() <= total {
                return 0;
            }
            total += trailing_count(record, record.len() - total);
        }
        bit -= 1;
    }
    if extra_flags & 1 == 1 {
        match record.len().checked_sub(total + 1) {
            Some(offset) => total += (record[offset] as usize & 0x3) + 1,
            None => return 0,
        }
    }
    total
}

/// The PalmDOC LZ77 decompression, exactly — the command byte is not a
/// flag byte: 0 and 0x09…0x7F are literals (so a record can begin with
/// literal `<html>`), 1…8 is a literal RUN of that length, 0xC0…0xFF is
/// "space + `byte ^ 0x80`", and only 0x80…0xBF is a back-reference.
///
/// A back-reference whose distance is zero, or reaches past the start of
/// the output, is skipped rather than clamped, exactly as the reference
/// implementations do: clamping would splice unrelated bytes into prose.
pub fn decompress_palmdoc(input: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> =
        Vec::with_capacity(input.len().saturating_mul(4).min(MAX_TEXT_BYTES));
    let mut index = 0usize;
    let count = input.len();
    while index < count {
        let command = input[index];
        index += 1;
        if (1..=8).contains(&command) {
            // A literal RUN of that many bytes.
            let mut remaining = command as usize;
            while remaining > 0 && index < count {
                out.push(input[index]);
                index += 1;
                remaining -= 1;
            }
        } else if command <= 0x7F {
            out.push(command);
        } else if command >= 0xC0 {
            // The writer's space-plus-ASCII shorthand.
            out.push(0x20);
            out.push(command ^ 0x80);
        } else if index < count {
            // 0x80…0xBF: a back reference. The low 3 bits are length − 3,
            // the upper 14 bits the distance.
            let value = ((command as u32) << 8) | input[index] as u32;
            index += 1;
            let distance = ((value & 0x3FFF) >> 3) as usize;
            let length = (value & 0x7) as usize + 3;
            if distance > 0 && distance <= out.len() {
                let mut source = out.len() - distance;
                for _ in 0..length {
                    let byte = out[source];
                    out.push(byte);
                    source += 1;
                }
            }
        }
        if out.len() > MAX_TEXT_BYTES {
            break;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------

/// The header's codepage first, then the CP1252 floor — a real book in any
/// encoding must come back as text, never an error. CP1250/1251 cover the
/// Cyrillic and Central-European books; every other codepage lands on
/// CP1252, whose table decodes every byte.
fn decode_text(data: &[u8], codepage: u32) -> String {
    match codepage {
        65001 => {
            if let Ok(text) = std::str::from_utf8(data) {
                return text.to_string();
            }
        }
        1251 => return super::bookdoc::decode_cp1251(data),
        1250 => return super::codepages::decode_cp1250(data),
        _ => {}
    }
    super::bookdoc::decode_cp1252(data)
}

// ---------------------------------------------------------------------------
// The book's HTML as items: headings, paragraphs, images
// ---------------------------------------------------------------------------

/// One thing the walk collected, in document order. Headings here are the
/// h1/h2 that open chapters; an h3+'s words are prose, exactly as the
/// reference reads them (books nest four deep, and every level opening a
/// chapter makes an unusable contents list).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Item {
    Heading(String),
    Paragraph(String),
    /// An `<img>` whose reference resolved to the pool — `name` is the
    /// `img/…` reference the chapter XHTML carries.
    Image { name: String, alt: Option<String> },
}

/// The prepared markup: `<script>`/`<style>`/`<head>` bodies become a
/// space, `<mbp:pagebreak/>` becomes a blank line. The same
/// element/pagebreak stripping the item walk does, so every chapter path
/// sees the same text.
fn prepare(html: &str) -> String {
    let bytes = html.as_bytes();
    let mut out = String::with_capacity(html.len());
    let mut at = 0usize;
    while at < bytes.len() {
        if bytes[at] == b'<' {
            if let Some((end, replacement)) = skipped_element_or_pagebreak(bytes, at) {
                out.push_str(replacement);
                at = end;
                continue;
            }
        }
        let ch = html[at..].chars().next().unwrap_or('\u{FFFD}');
        out.push(ch);
        at += ch.len_utf8();
    }
    out
}

/// `<script|style|head …>…</name>` → one space; `<mbp:pagebreak/>` → a
/// blank line. Returns the end offset and the replacement.
fn skipped_element_or_pagebreak(bytes: &[u8], at: usize) -> Option<(usize, &'static str)> {
    // The pagebreak first: its colon breaks the plain-name scan below.
    let head = b"<mbp:pagebreak";
    if at + head.len() <= bytes.len() && bytes[at..at + head.len()].eq_ignore_ascii_case(head) {
        let mut q = at + head.len();
        while q < bytes.len() && bytes[q].is_ascii_whitespace() {
            q += 1;
        }
        if q < bytes.len() && bytes[q] == b'/' {
            q += 1;
        }
        if q < bytes.len() && bytes[q] == b'>' {
            return Some((q + 1, "\n\n"));
        }
        return None;
    }
    let mut p = at + 1;
    let name = tag_name_at(bytes, &mut p)?;
    if p >= bytes.len() {
        return None;
    }
    match name {
        b"script" | b"style" | b"head" => {
            // The matching close: `</name`, then to its '>'.
            let mut needle = Vec::with_capacity(2 + name.len());
            needle.extend_from_slice(b"</");
            needle.extend_from_slice(name);
            let close = find_ci(bytes, p, &needle)?;
            let mut end = close + 2 + name.len();
            while end < bytes.len() && bytes[end] != b'>' {
                end += 1;
            }
            Some((end + 1, " "))
        }
        _ => None,
    }
}

/// The lowercased tag-name run starting at `*p` (after any `/`), consuming
/// `p` past it. `None` when there is no name.
fn tag_name_at<'b>(bytes: &'b [u8], p: &mut usize) -> Option<&'b [u8]> {
    let mut at = *p;
    if bytes.get(at) == Some(&b'/') {
        at += 1;
    }
    let start = at;
    while at < bytes.len() && bytes[at].is_ascii_alphanumeric() {
        at += 1;
    }
    if at == start {
        return None;
    }
    *p = at;
    Some(&bytes[start..at])
}

/// The next occurrence of an ASCII needle, case-insensitively.
fn find_ci(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    let mut at = from;
    while at + needle.len() <= haystack.len() {
        if haystack[at..at + needle.len()].eq_ignore_ascii_case(needle) {
            return Some(at);
        }
        at += 1;
    }
    None
}

/// Flattens the book's HTML into heading/paragraph/image items.
///
/// Mobipocket books are HTML, so the markup IS the structure: `<h1>`–`<h2>`
/// open chapters, `<h3>`+ stay inside the current chapter as prose, and
/// block tags separate paragraphs. Hard-wrapped source lines are rejoined
/// into paragraphs — Gutenberg wraps at ~70 columns, so splitting on
/// newlines would shred every sentence. What is left after the boundary
/// tags are INLINE tags (font, em, span, a…) — they vanish, because
/// browsers render inline markup without whitespace and replacing it with
/// a space shredded words (`<font>a</font>sh` decoded as "a sh" — the
/// artifact the word-diff against calibre's own conversion caught).
fn items(from: &str) -> Vec<Item> {
    let bytes = from.as_bytes();
    let mut items: Vec<Item> = Vec::new();
    // A heading OPENING tag (`<h1 …>`) vs. its CLOSING tag (`</h1>`): both
    // match the boundary pattern, and treating the closing tag as an
    // opening one is what made every fixture come back untitled.
    let mut pending_heading = false;
    let mut paragraph: Vec<String> = Vec::new();
    let mut cursor = 0usize;

    while let Some((start, end, boundary)) = next_boundary(bytes, cursor) {
        // A boundary that starts BEFORE the cursor would make the chunk a
        // reversed range — a truncated back reference in the decompressed
        // text can produce an unbalanced tag whose boundary overlaps the
        // previous one. Skip it and keep the cursor where it is.
        if start < cursor {
            continue;
        }
        let chunk = &from[cursor..start];
        cursor = end;
        let text = sanitize(&decode_entities_lenient(&strip_tags(chunk)));

        match boundary {
            Boundary::HeadingOpen(level) => {
                // An opening heading tag ends whatever came before it; the
                // text inside is its title.
                flush_paragraph(&mut items, &mut paragraph, &mut pending_heading);
                if !text.is_empty() {
                    if level <= 2 {
                        items.push(Item::Heading(text));
                    } else {
                        // h3+ nests inside the chapter: its words are prose.
                        paragraph.push(text);
                    }
                }
                pending_heading = level <= 2;
            }
            Boundary::Image { tag } => {
                // An image is a boundary even without a close tag — an
                // alt-less image must not glue two words together.
                flush_paragraph(&mut items, &mut paragraph, &mut pending_heading);
                let src = attribute_of(&tag, "src").unwrap_or_default();
                if !src.is_empty() {
                    let alt = attribute_of(&tag, "alt").filter(|a| !a.is_empty());
                    items.push(Item::Image { name: src, alt });
                }
            }
            Boundary::Block => {
                if !text.is_empty() {
                    if pending_heading {
                        items.push(Item::Heading(text));
                        pending_heading = false;
                    } else {
                        paragraph.push(text);
                    }
                }
                // Every other boundary ends a block: `</p>`, `<br/>`, `</h2>`, …
                flush_paragraph(&mut items, &mut paragraph, &mut pending_heading);
            }
        }
    }

    let tail = sanitize(&decode_entities_lenient(&strip_tags(&from[cursor..])));
    if !tail.is_empty() {
        if pending_heading {
            items.push(Item::Heading(tail));
        } else {
            paragraph.push(tail);
        }
    }
    flush_paragraph(&mut items, &mut paragraph, &mut pending_heading);
    items
}

fn flush_paragraph(items: &mut Vec<Item>, paragraph: &mut Vec<String>, pending: &mut bool) {
    if paragraph.is_empty() {
        return;
    }
    let joined = sanitize(&paragraph.join(" "));
    paragraph.clear();
    if joined.is_empty() {
        return;
    }
    if *pending {
        items.push(Item::Heading(joined));
        *pending = false;
    } else {
        items.push(Item::Paragraph(joined));
    }
}

enum Boundary {
    Block,
    HeadingOpen(usize),
    /// An `<img …>` whose tag text rides along for the attribute reader.
    Image { tag: String },
}

/// One structural boundary at or after `from`: a block tag's open or
/// close, an h1–h6 opening (with its level), or an `<img>`. Returns the
/// tag's start, the offset past its `>`, and the kind.
fn next_boundary(bytes: &[u8], from: usize) -> Option<(usize, usize, Boundary)> {
    let mut at = from;
    while at < bytes.len() {
        if bytes[at] != b'<' {
            at += 1;
            continue;
        }
        let mut p = at + 1;
        let closing = bytes.get(p) == Some(&b'/');
        if closing {
            p += 1;
        }
        let name = tag_name_at(bytes, &mut p);
        // The name must end at a non-word byte — `\b` in the reference's
        // pattern. `<param>` is not a `<p>`; `<h10>` is not an `<h1>`.
        if matches!(bytes.get(p), Some(b) if b.is_ascii_alphanumeric() || *b == b'_') {
            at += 1;
            continue;
        }
        let kind = match name {
            Some(b"img") if !closing => Boundary::Image {
                tag: String::from_utf8_lossy(bytes.get(at..tag_end(bytes, p)? + 1)?).into_owned(),
            },
            Some(b"br") if !closing => {
                // `<br\s*/?>` — only whitespace, an optional slash, then
                // '>' counts; `<br class="x">` is inline markup, not a
                // boundary, in the reference's pattern too.
                let mut q = p;
                while q < bytes.len() && bytes[q].is_ascii_whitespace() {
                    q += 1;
                }
                if q < bytes.len() && bytes[q] == b'/' {
                    q += 1;
                }
                if bytes.get(q) != Some(&b'>') {
                    at += 1;
                    continue;
                }
                Boundary::Block
            }
            Some(name)
                if name.len() == 2 && name[0] == b'h' && (b'1'..=b'6').contains(&name[1]) =>
            {
                if closing {
                    Boundary::Block
                } else {
                    Boundary::HeadingOpen((name[1] - b'0') as usize)
                }
            }
            Some(name) if BLOCK_TAGS.contains(&name) => Boundary::Block,
            _ => {
                at += 1;
                continue;
            }
        };
        // The tag runs to its '>' (`[^>]*>` — no '>' inside, the same
        // limitation the reference's pattern had).
        let end = match &kind {
            Boundary::Image { .. } => tag_end(bytes, p)? + 1,
            _ => p + bytes[p..].iter().position(|&b| b == b'>')? + 1,
        };
        return Some((at, end, kind));
    }
    None
}

/// The offset of the '>' that closes the tag whose name ends at `p`.
fn tag_end(bytes: &[u8], p: usize) -> Option<usize> {
    Some(p + bytes[p..].iter().position(|&b| b == b'>')?)
}

const BLOCK_TAGS: &[&[u8]] = &[
    b"p",
    b"div",
    b"li",
    b"tr",
    b"blockquote",
    b"section",
    b"article",
    b"dd",
    b"dt",
    b"pre",
    b"figcaption",
];

/// Every remaining `<…>` run the walk did not name: what is left after the
/// boundary tags are inline tags (font, em, span, a…).
fn strip_tags(chunk: &str) -> String {
    let bytes = chunk.as_bytes();
    let mut out = String::with_capacity(chunk.len());
    let mut at = 0usize;
    while at < bytes.len() {
        if bytes[at] == b'<' {
            // `<[^>]{0,400}>` — a '>' beyond 400 bytes does not close the
            // "tag"; the '<' stays as text, as in the reference.
            let limit = (at + 1 + 400).min(bytes.len());
            if let Some(gt) = bytes[at + 1..limit].iter().position(|&b| b == b'>') {
                at += gt + 2;
                continue;
            }
        }
        let ch = chunk[at..].chars().next().unwrap_or('\u{FFFD}');
        out.push(ch);
        at += ch.len_utf8();
    }
    out
}

/// One attribute of a tag, entity-decoded: `name="value"`, `'value'` or a
/// bare word.
fn attribute_of(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let needle = format!("{name}=");
    let at = lower.find(&needle)? + needle.len();
    let rest = &tag[at..];
    let quote = rest.chars().next()?;
    let value = if quote == '"' || quote == '\'' {
        let end = rest[1..].find(quote)? + 1;
        &rest[1..end]
    } else {
        rest.split(|c: char| c.is_whitespace() || c == '>').next()?
    };
    Some(decode_entities_lenient(value))
}

/// Entities decoded once — the text the chapter XHTML carries is written
/// back escaped by the builders, so `&amp;` in the source must not become
/// `&amp;amp;`.
fn decode_entities_lenient(text: &str) -> String {
    super::xml::decode_entities(text).unwrap_or_else(|| text.to_string())
}

/// Gutenberg's editorial furniture, which a reader would never say:
/// `[Illustration]`, `[Pg 42]`, footnote markers, emphasis marks, the
/// glyph references encrypted-font files leave behind — and the control
/// bytes an engine chokes on. Whitespace runs collapse here too, the way
/// the other readers' `tidy` does.
fn sanitize(text: &str) -> String {
    let out = strip_editorial_markers(text);
    let out = strip_cid_references(&out);
    // Emphasis: keep the words, drop the marks. Every `*` goes — a run of
    // four is two matches of the reference's `\*{1,3}` either way.
    let out: String = out.chars().filter(|&c| c != '*').collect();
    let out = strip_underscore_emphasis(&out);
    let out = out.replace("────", " ").replace("————", " ");
    let out = out.replace('\u{0}', "");
    tidy(&out)
}

/// `[Illustration]`, `[Pg 42]`, `[Footnote …]` and `[Transcriber's note …]`
/// markers → a space. Case-insensitive, the way the reference's pattern is.
fn strip_editorial_markers(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut at = 0usize;
    while at < bytes.len() {
        if bytes[at] == b'[' {
            if let Some(end) = text[at..].find(']') {
                if is_editorial_marker(&text[at + 1..at + end]) {
                    out.push(' ');
                    at += end + 1;
                    continue;
                }
            }
        }
        let ch = text[at..].chars().next().unwrap_or('\u{FFFD}');
        out.push(ch);
        at += ch.len_utf8();
    }
    out
}

fn is_editorial_marker(inner: &str) -> bool {
    let lower = inner.trim().to_ascii_lowercase();
    if lower == "illustration" {
        return true;
    }
    if let Some(rest) = lower.strip_prefix("pg ") {
        return !rest.is_empty()
            && rest
                .chars()
                .all(|c| c.is_ascii_digit() || "ivxlcdm".contains(c));
    }
    lower.starts_with("footnote") || lower.starts_with("transcriber")
}

/// `(cid:123)` — the glyph references encrypted-font files leave behind.
fn strip_cid_references(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut at = 0usize;
    while at < bytes.len() {
        if bytes[at] == b'(' && text[at..].starts_with("(cid:") {
            if let Some(end) = text[at..].find(')') {
                if text[at + 5..at + end].bytes().all(|b| b.is_ascii_digit()) {
                    at += end + 1;
                    continue;
                }
            }
        }
        let ch = text[at..].chars().next().unwrap_or('\u{FFFD}');
        out.push(ch);
        at += ch.len_utf8();
    }
    out
}

/// Runs of 1–3 underscores with a non-letter/non-digit on both sides are
/// emphasis marks, not words-with-underscores (`snake_case` keeps its). A
/// run of four or more matches nowhere — every candidate has an underscore
/// for a neighbour — the way the reference's lookarounds behave.
fn strip_underscore_emphasis(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut at = 0usize;
    while at < bytes.len() {
        if bytes[at] == b'_' {
            let mut run = 0usize;
            while run < 3 && bytes.get(at + run) == Some(&b'_') {
                run += 1;
            }
            let before_ok = !text[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric());
            let after_ok = text[at + run..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric());
            if before_ok && after_ok {
                at += run;
                continue;
            }
        }
        let ch = text[at..].chars().next().unwrap_or('\u{FFFD}');
        out.push(ch);
        at += ch.len_utf8();
    }
    out
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
// Images: recindex / kindle:embed → the pool
// ---------------------------------------------------------------------------

/// Every image the text references, in first-use order, with the `<img>`
/// tags rewritten to the pool's names.
///
/// `recindex` is 1-based over the resource records that follow the text
/// records; writers disagree about the exact base — both the header's
/// first image record and the mobiunpack convention
/// (`firstResource + n − 1`) are tried, and whichever lands on a real
/// image payload wins. KF8 books spell the same reference
/// `kindle:embed:NNNN`. A reference that lands on nothing drops its image;
/// the text never breaks.
fn extract_images(
    html: &str,
    pdb: &PalmDatabase,
    header: &MobiHeader,
    base: usize,
) -> (String, Vec<BookImage>) {
    let first_resource = 1 + header.text_record_count;
    let mut pool: Vec<BookImage> = Vec::new();
    let mut pool_by_record: HashMap<usize, usize> = HashMap::new();
    let bytes = html.as_bytes();
    let mut rewritten = String::with_capacity(html.len());
    let mut at = 0usize;

    while at < bytes.len() {
        // Copy the text up to the next `<img` tag.
        let Some(tag_at) = find_img_tag(bytes, at) else {
            rewritten.push_str(&html[at..]);
            break;
        };
        rewritten.push_str(&html[at..tag_at]);
        // The tag runs to its '>' — or ends the document, in which case it
        // is not a tag this reader can rewrite.
        let Some(gt) = bytes[tag_at..].iter().position(|&b| b == b'>') else {
            rewritten.push_str(&html[tag_at..]);
            break;
        };
        let tag = &html[tag_at..tag_at + gt + 1];
        at = tag_at + gt + 1;
        let Some(index) =
            referenced_image(tag, pdb, header, base, first_resource, &mut pool, &mut pool_by_record)
        else {
            continue; // unresolvable: the tag is dropped
        };
        let mut tag_out = format!("<img src=\"{}\"", image_ref_name(index));
        if let Some(alt) = attribute_of(tag, "alt").filter(|a| !a.is_empty()) {
            tag_out.push_str(&format!(" alt=\"{}\"", xml_escape(&alt)));
        }
        tag_out.push('>');
        rewritten.push_str(&tag_out);
    }
    (rewritten, pool)
}

/// The next `<img` tag at or after `from` — `img` as a whole tag name, so
/// `<imgsrc>` is text but `<IMG src=…>` counts.
fn find_img_tag(bytes: &[u8], from: usize) -> Option<usize> {
    let mut at = from;
    while let Some(lt) = bytes[at..].iter().position(|&b| b == b'<').map(|p| p + at) {
        if lt + 4 <= bytes.len() && bytes[lt + 1..lt + 4].eq_ignore_ascii_case(b"img") {
            let next = bytes.get(lt + 4);
            if next.is_none_or(|b| b.is_ascii_whitespace() || *b == b'>' || *b == b'/') {
                return Some(lt);
            }
        }
        at = lt + 1;
    }
    None
}

/// The pool index an `<img>` tag's reference resolves to, adding the
/// record's payload to the pool when it is new.
fn referenced_image(
    tag: &str,
    pdb: &PalmDatabase,
    header: &MobiHeader,
    base: usize,
    first_resource: usize,
    pool: &mut Vec<BookImage>,
    pool_by_record: &mut HashMap<usize, usize>,
) -> Option<usize> {
    let number = recindex_of(tag).or_else(|| embed_of(tag))?;
    // Bases to try, in order: the header's own first image record (KF8
    // books, and MOBI6 books whose writer set it), then the record after
    // the text (the mobiunpack convention), then the raw one-based guess.
    let mut candidates: Vec<usize> = Vec::new();
    if header.first_image_index > 0 {
        candidates.push(base + header.first_image_index + number - 1);
    }
    candidates.push(base + first_resource + number - 1);
    candidates.push(base + first_resource + number);
    for candidate in candidates {
        let Some(record) = pdb.record(candidate) else { continue };
        let Some((mime, data)) = image_payload(record) else { continue };
        if let Some(&index) = pool_by_record.get(&candidate) {
            return Some(index);
        }
        pool_by_record.insert(candidate, pool.len());
        pool.push(BookImage {
            name: image_ref_name(pool.len()),
            mime: mime.to_string(),
            data: data.to_vec(),
        });
        return Some(pool.len() - 1);
    }
    None
}

fn recindex_of(tag: &str) -> Option<usize> {
    let lower = tag.to_ascii_lowercase();
    let at = lower.find("recindex")? + "recindex".len();
    let rest = tag[at..].trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    let value = if let Some(q) = rest.strip_prefix('"') {
        q.split('"').next()?
    } else if let Some(q) = rest.strip_prefix('\'') {
        q.split('\'').next()?
    } else {
        rest.split(|c: char| c.is_whitespace() || c == '>').next()?
    };
    value.parse::<usize>().ok().filter(|n| *n > 0)
}

fn embed_of(tag: &str) -> Option<usize> {
    let lower = tag.to_ascii_lowercase();
    let at = lower.find("kindle:embed:")? + "kindle:embed:".len();
    let digits: String = tag[at..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse::<usize>().ok().filter(|n| *n > 0)
}

/// The `img/` reference name for pool index `index` — `000000`, `000001`,
/// in order of first reference. Recindex numbers are arbitrary per book;
/// the book's own names are flat and safe in a URL path.
fn image_ref_name(index: usize) -> String {
    format!("img/{index:06}")
}

/// A record's bytes when they are a decodable image payload (JPEG, PNG or
/// GIF), with the mime the reader serves them under. A record that is none
/// of these is index or layout furniture, and writing it as `cover.jpg`
/// would produce an undecodable file.
fn image_payload(record: &[u8]) -> Option<(&'static str, &[u8])> {
    if record.len() <= 8 {
        return None;
    }
    let is_jpeg = record[0] == 0xFF && record[1] == 0xD8;
    let is_png = record[0] == 0x89 && record[1] == 0x50 && record[2] == 0x4E && record[3] == 0x47;
    let is_gif = record[0] == 0x47 && record[1] == 0x49 && record[2] == 0x46;
    if is_jpeg {
        Some(("image/jpeg", record))
    } else if is_png {
        Some(("image/png", record))
    } else if is_gif {
        Some(("image/gif", record))
    } else {
        None
    }
}

/// The book's cover image record.
///
/// Order of preference: the EXTH 201 cover offset (the writer's own
/// declaration), the EXTH 202 thumbnail offset (calibre writes it), then
/// the first record that actually IS an image — the resource area holds an
/// index, a FLIS/FCIS pair and a DCTL before the pictures, in no
/// guaranteed order, so a magic-byte check against "first record" gives up
/// on the index. A KF8's first image may be a small publisher logo rather
/// than the cover; that is the writer's choice, honored as declared.
///
/// The 201/202 offsets count from the header's first image record —
/// calibre names its cover `first_image_index + cover_offset` the same
/// way — falling back to the record after the text on writers that left
/// the field unset.
fn cover_image<'p>(
    pdb: &'p PalmDatabase,
    header: &MobiHeader,
    base: usize,
    exth: &Exth,
) -> Option<(&'static str, &'p [u8])> {
    let image_base = match header.first_image_index {
        0 => base + 1 + header.text_record_count,
        first_image => base + first_image,
    };
    for offset in [exth.cover_offset, exth.thumbnail_offset] {
        if offset < 0 {
            continue;
        }
        if let Some(record) = pdb.record(image_base + offset as usize) {
            if let Some(payload) = image_payload(record) {
                return Some(payload);
            }
        }
    }
    for index in (base + 1 + header.text_record_count)..pdb.record_count() {
        if let Some(record) = pdb.record(index) {
            if let Some(payload) = image_payload(record) {
                return Some(payload);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Chapters
// ---------------------------------------------------------------------------

/// One chapter cut from the book: its title and its XHTML body.
type Chapter = (Option<String>, String);

/// Splits the book into chapters, by the first flow that applies.
///
/// * **The book's own contents** comes first when the index carried two or
///   more placeable entries: the publisher's own contents page is the
///   authority on where chapters start and what they are called, and the
///   heading rule cannot see it (a contents page rendered as links into
///   one long section has no headings at all).
/// * **KF8 rawML** is a sequence of XHTML "flows" — `<html>…</html>` — and
///   a reference book's flows carry 165 h1s and 265 h2s (the device
///   report: "the division of chapters is not accurate"). The flow
///   boundary is the book's own structure; a flow's title is its first
///   h1, or its `epub:type` for front matter.
/// * **MOBI6** keeps the heading rule: chapters at level-1/2 headings, h3+
///   staying inside as prose. A book with no headings at all (older
///   conversions) chunks into bounded spans of paragraphs.
fn chapters(prepared: &str, is_kf8: bool, toc: &[TocEntry]) -> (Vec<Chapter>, Vec<BookToc>) {
    if toc.len() >= 2 {
        let (list, rows) = toc_chapters(prepared, toc);
        if !list.is_empty() {
            return (list, rows);
        }
    }
    let list = if is_kf8 {
        let flows = kf8_flow_chapters(prepared);
        if !flows.is_empty() {
            flows
        } else {
            heading_chapters(prepared)
        }
    } else {
        heading_chapters(prepared)
    };
    let rows = list
        .iter()
        .enumerate()
        .map(|(index, (title, _))| BookToc {
            label: title
                .clone()
                .unwrap_or_else(|| format!("Chapter {}", index + 1)),
            chapter_index: index,
            depth: 0,
        })
        .collect();
    (list, rows)
}

/// Cuts chapters at the index's own byte offsets in the prepared text,
/// labelling each with the publisher's words. Offsets that fall inside a
/// tag are snapped forward to the next tag boundary, so a chapter never
/// opens mid-tag; entries whose offset cannot be placed are dropped rather
/// than guessed at.
fn toc_chapters(prepared: &str, toc: &[TocEntry]) -> (Vec<Chapter>, Vec<BookToc>) {
    let bytes = prepared.as_bytes();
    // Placeable offsets only, in document order, deduplicated.
    let mut cuts: Vec<(String, usize, usize)> = Vec::new(); // (label, start, depth)
    for entry in toc {
        let Some(snapped) = snap_offset(entry.offset, bytes) else {
            continue;
        };
        if cuts.last().is_some_and(|(_, start, _)| *start == snapped) {
            // Two entries naming the same byte (a part and its part-title):
            // keep the FIRST label — the publisher's order is the one the
            // contents page shows.
            continue;
        }
        cuts.push((entry.label.clone(), snapped, entry.depth));
    }
    if cuts.len() < 2 {
        return (Vec::new(), Vec::new());
    }

    let mut list: Vec<Chapter> = Vec::new();
    let mut rows: Vec<BookToc> = Vec::new();
    for (index, (label, start, depth)) in cuts.iter().enumerate() {
        if list.len() >= MAX_CHAPTERS {
            break;
        }
        let end = cuts
            .get(index + 1)
            .map(|(_, next, _)| *next)
            .unwrap_or(bytes.len());
        if end <= *start {
            continue;
        }
        let segment = &prepared[*start..end];
        let (_, html) = segment_chapter(&items(segment), Some(label));
        if html.is_empty() {
            continue;
        }
        rows.push(BookToc {
            label: label.clone(),
            chapter_index: list.len(),
            depth: *depth,
        });
        // The publisher's label wins: it is what the contents page says.
        list.push((Some(label.clone()), html));
    }
    (list, rows)
}

/// Snaps a byte offset to a tag boundary at or after it. An offset inside
/// a tag (or inside text) walks forward to the next `<` that opens an
/// element, so the chapter's markup stays well-formed; `None` when nothing
/// plausible follows, which drops the entry.
fn snap_offset(offset: usize, bytes: &[u8]) -> Option<usize> {
    if offset >= bytes.len() {
        return None;
    }
    if bytes[offset] == b'<' {
        return Some(offset); // already at '<'
    }
    let mut cursor = offset;
    while cursor < bytes.len() {
        if bytes[cursor] == b'<' && cursor + 1 < bytes.len() {
            let next = bytes[cursor + 1];
            // An element open (or a comment), not a stray '<' the
            // back-reference corruption leaves behind.
            if next.is_ascii_alphabetic() || next == b'/' || next == b'!' {
                return Some(cursor);
            }
        }
        cursor += 1;
    }
    None
}

/// KF8: one chapter per flow. The flow's `<html>…</html>` wrapper and its
/// following bare content (the section markup a KF8 writer puts after
/// `</html>`) both belong to the chapter — the walk rebuilds the body
/// XHTML, so the wrapper itself cannot leak into a chapter the way it
/// stopped the reference's generated EPUB from rendering. The title is the
/// chapter's own first `<h1>` text, never invented; front-matter flows
/// without an h1 take their `epub:type` (titlepage, copyright-page, toc…)
/// as a title.
fn kf8_flow_chapters(prepared: &str) -> Vec<Chapter> {
    let bytes = prepared.as_bytes();
    // The flow OPEN tags; the flows must actually tile the book — a single
    // stray <html>…</html> inside one MOBI6-style document is not a KF8
    // structure, and cutting it would throw away everything around it.
    let mut boundaries: Vec<usize> = Vec::new();
    let mut at = 0usize;
    while let Some(start) = find_ci(bytes, at, b"<html") {
        // A whole tag name: `<htmlbody>` is text.
        let after = start + 5;
        if bytes
            .get(after)
            .is_none_or(|b| !b.is_ascii_alphanumeric() && *b != b'_')
        {
            boundaries.push(start);
        }
        at = start + 5;
    }
    if boundaries.len() < 2 {
        return Vec::new();
    }

    let mut result: Vec<Chapter> = Vec::new();
    for (index, start) in boundaries.iter().enumerate() {
        if result.len() >= MAX_CHAPTERS {
            break;
        }
        let end = boundaries
            .get(index + 1)
            .copied()
            .unwrap_or(prepared.len());
        let segment = &prepared[*start..end];
        let its = items(segment);
        if its.is_empty() {
            continue;
        }
        let (mut title, html) = segment_chapter(&its, None);
        if title.is_none() {
            title = kf8_section_title(segment);
        }
        // A titleless flow with nothing in it is not a chapter; the same
        // heading twice in a row with nothing between is the writer
        // re-stating the title (the chapter label the generator repeats
        // inside the section) — not two chapters.
        if title.is_none() && html.is_empty() {
            continue;
        }
        if html.is_empty() && result.last().is_some_and(|(last, _)| *last == title) {
            continue;
        }
        result.push((title, html));
    }
    result
}

/// A title for a front-matter flow with no `<h1>`: its own
/// `epub:type="…"` on the section element ("titlepage", "toc", …) —
/// capitalized for the contents list. `None` when the flow declares
/// neither.
fn kf8_section_title(segment: &str) -> Option<String> {
    let lower = segment.to_ascii_lowercase();
    let at = lower.find("epub:type=\"")? + "epub:type=\"".len();
    let raw = segment.get(at..)?.split('"').next()?;
    if raw.is_empty() || !raw.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
        return None;
    }
    let readable = raw.replace('-', " ");
    let mut chars = readable.chars();
    let head = chars.next()?.to_uppercase().collect::<String>();
    Some(head + chars.as_str())
}

/// MOBI6: the heading-delimited path (KF8's fallback too, for a book whose
/// flows did not tile).
///
/// Chapters cut at the level-1/2 heading tags — the same boundaries the
/// item walk opens chapters at. A book with no headings at all chunks into
/// [`CHAPTER_BLOCK_LIMIT`]-item spans so the one-chapter-at-a-time memory
/// bound still holds, and the section title carries onto the continuation
/// so the contents list never shows "Chapter N" mid-section.
fn heading_chapters(prepared: &str) -> Vec<Chapter> {
    let segments = chapter_segments(prepared);
    if segments.len() <= 1 {
        let its = items(prepared);
        if its.is_empty() {
            return whole_text_chapter(prepared);
        }
        let chunked = chunked_chapters(&its);
        if !chunked.is_empty() {
            return chunked;
        }
        // Only headings, no prose: one chapter of the whole text.
        return whole_text_chapter(prepared);
    }

    let mut result: Vec<Chapter> = Vec::new();
    for segment in segments {
        if result.len() >= MAX_CHAPTERS {
            break;
        }
        let its = items(segment);
        if its.is_empty() {
            continue;
        }
        // A titleless segment that only repeats the previous chapter's
        // title is the writer re-stating the header, not new content.
        if let [Item::Paragraph(text)] = &its[..] {
            if result
                .last()
                .is_some_and(|(last, _)| last.as_deref() == Some(text.as_str()))
            {
                continue;
            }
        }
        let (title, html) = segment_chapter(&its, None);
        if title.is_none() && html.is_empty() {
            continue;
        }
        result.push((title, html));
    }
    result
}

/// A book with no structure at all: one chapter of the whole text, when
/// there is any.
fn whole_text_chapter(prepared: &str) -> Vec<Chapter> {
    let whole = sanitize(&decode_entities_lenient(&strip_tags(prepared)));
    if whole.is_empty() {
        Vec::new()
    } else {
        vec![(None, format!("<p>{}</p>", xml_escape(&whole)))]
    }
}

/// The prepared markup cut at the level-1/2 heading tags — the same
/// boundaries the item walk opens chapters at.
fn chapter_segments(prepared: &str) -> Vec<&str> {
    let bytes = prepared.as_bytes();
    let mut starts: Vec<usize> = Vec::new();
    let mut at = 0usize;
    while let Some((start, end, boundary)) = next_boundary(bytes, at) {
        at = end;
        if let Boundary::HeadingOpen(level) = boundary {
            if level <= 2 {
                starts.push(start);
            }
        }
    }
    if starts.len() < 2 {
        return vec![prepared];
    }
    let mut segments: Vec<&str> = Vec::new();
    let mut cursor = 0usize;
    for start in starts {
        if start > cursor {
            segments.push(&prepared[cursor..start]);
        } else if segments.is_empty() {
            segments.push("");
        }
        cursor = start;
    }
    if cursor < prepared.len() {
        segments.push(&prepared[cursor..]);
    }
    segments
}

/// The heading-less fallback: bounded spans of items, the section title
/// carried onto the continuation chapter so the contents list never shows
/// "Chapter N" mid-section.
fn chunked_chapters(its: &[Item]) -> Vec<Chapter> {
    let mut result: Vec<Chapter> = Vec::new();
    let mut title: Option<String> = None;
    let mut html = String::new();
    let mut count = 0usize;
    for item in its {
        if result.len() >= MAX_CHAPTERS {
            break;
        }
        match item {
            Item::Heading(text) => {
                flush_chapter(&mut result, &mut title, &mut html);
                title = Some(text.clone());
            }
            Item::Paragraph(text) => {
                html.push_str("<p>");
                html.push_str(&xml_escape(text));
                html.push_str("</p>");
                count += 1;
            }
            Item::Image { name, alt } => push_image(&mut html, name, alt.as_deref()),
        }
        if count >= CHAPTER_BLOCK_LIMIT {
            // The bound: the section title carries onto the continuation.
            let carried = title.take();
            flush_chapter(&mut result, &mut title, &mut html);
            title = carried;
            count = 0;
        }
    }
    flush_chapter(&mut result, &mut title, &mut html);
    result
}

fn flush_chapter(result: &mut Vec<Chapter>, title: &mut Option<String>, html: &mut String) {
    if !html.is_empty() {
        result.push((title.take(), std::mem::take(html)));
    }
}

/// One chapter's build from its items: the first heading is the title, the
/// rest is prose (a segment opened with this heading, so anything after is
/// prose — a malformed second h1/h2 is prose, not a chapter split). A
/// paragraph that merely echoes the title — the writer restating the
/// chapter label inside the section — is dropped from the prose. Images
/// keep their places in the flow.
fn segment_chapter(its: &[Item], forced_label: Option<&str>) -> (Option<String>, String) {
    // One emit per item, in document order.
    enum Emit<'a> {
        Paragraph(&'a str),
        Image { name: &'a str, alt: Option<&'a str> },
    }
    let mut emits: Vec<Emit> = Vec::with_capacity(its.len());
    let mut title: Option<String> = None;
    for item in its {
        match item {
            Item::Heading(text) => {
                if title.is_none() {
                    title = Some(text.clone());
                } else {
                    emits.push(Emit::Paragraph(text));
                }
            }
            Item::Paragraph(text) => emits.push(Emit::Paragraph(text)),
            Item::Image { name, alt } => emits.push(Emit::Image { name, alt: alt.as_deref() }),
        }
    }
    // The echo: the first paragraph repeating the title (or the
    // publisher's label) rides out.
    let echo_against = forced_label.or(title.as_deref());
    let echo_at = echo_against.and_then(|against| {
        emits.iter().position(
            |emit| matches!(emit, Emit::Paragraph(text) if *text == against),
        )
    });
    let mut html = String::new();
    for (position, emit) in emits.iter().enumerate() {
        if Some(position) == echo_at {
            continue;
        }
        match emit {
            Emit::Paragraph(text) => {
                html.push_str("<p>");
                html.push_str(&xml_escape(text));
                html.push_str("</p>");
            }
            Emit::Image { name, alt } => push_image(&mut html, name, *alt),
        }
    }
    (title, html)
}

fn push_image(html: &mut String, name: &str, alt: Option<&str>) {
    html.push_str(&format!("<img src=\"{name}\""));
    if let Some(alt) = alt {
        html.push_str(&format!(" alt=\"{}\"", xml_escape(alt)));
    }
    html.push('>');
}

// ---------------------------------------------------------------------------
// The book's own contents: INDX / TAGX / CNCX
// ---------------------------------------------------------------------------

/// One entry of the book's own table of contents.
///
/// `offset` is a byte offset into the decompressed text stream — the
/// format's own coordinate for "the chapter starts here", which is what a
/// reader must cut chapters at when the markup's headings disagree with
/// the publisher's contents page. Never invented: the label is the
/// publisher's words from the CNCX, and the offset is the index's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TocEntry {
    pub label: String,
    pub offset: usize,
    /// Nesting depth as the index declares it (0 = top level).
    pub depth: usize,
}

/// The book's own table of contents, read from its index records.
///
/// The chain, in the format's own order:
///
/// 1. **INDX** — the index record the MOBI header names at 0xF4. Its TAGX
///    block maps tag numbers to (values, mask) pairs; the control bytes of
///    every indexed entry say which tags it carries.
/// 2. **The index's continuation records** — the number the INDX header
///    declares, each with its own IDXT entry table.
/// 3. **CNCX** — the records after the continuations (the header's `ncncx`
///    says how many): bare vwi-length-prefixed strings, addressed by their
///    byte offset within the concatenated blob.
///
/// An entry's tag 1 is the byte offset into the decompressed text, tag 3
/// the CNCX offset of its label, tag 4 its nesting depth.
///
/// Every step is optional in the wild (writers omit the index, point the
/// header at nothing, or ship an empty CNCX), so each reader returns empty
/// rather than throwing — the caller falls back to the heading paths and
/// the book still imports.
fn table_of_contents(pdb: &PalmDatabase, header: &MobiHeader, base: usize) -> Vec<TocEntry> {
    if header.ncx_index == 0 {
        return Vec::new();
    }
    let ncx = base + header.ncx_index;
    let Some(index_record) = pdb.record(ncx).filter(|r| r.len() > 4 && &r[..4] == b"INDX") else {
        return Vec::new();
    };
    let Some((control_byte_count, tags)) = tagx_block(index_record) else {
        return Vec::new();
    };
    let continuation_count = u32(index_record, 0x18) as usize;
    let ncncx = u32(index_record, 0x34) as usize;
    let codepage = u32(index_record, 0x1C);
    let strings = cncx_strings(pdb, ncx + continuation_count + 1, ncncx, codepage);

    let mut entries: Vec<TocEntry> = Vec::new();
    for continuation in 1..=continuation_count {
        let Some(record) = pdb
            .record(ncx + continuation)
            .filter(|r| r.len() > 4 && &r[..4] == b"INDX")
        else {
            continue;
        };
        let count = u32(record, 0x18) as usize;
        let idxt = u32(record, 0x14) as usize;
        if idxt < 4 || idxt + 4 + 2 * count > record.len() {
            continue;
        }
        for e in 0..count {
            let start = u16(record, idxt + 4 + 2 * e) as usize;
            // The last entry ends at the IDXT block; zero-fill may sit
            // between.
            let end = if e + 1 < count {
                u16(record, idxt + 4 + 2 * (e + 1)) as usize
            } else {
                idxt
            };
            if start >= end || end > record.len() {
                continue;
            }
            if let Some(entry) = toc_entry(&record[start..end], control_byte_count, &tags, &strings)
            {
                entries.push(entry);
                if entries.len() >= MAX_CHAPTERS {
                    return entries;
                }
            }
        }
    }
    entries
}

/// The TAGX block inside an INDX: `TAGX`, a u32 first-entry offset, a u32
/// control-byte count, then 4-byte `[tag, values, mask, eof]` entries up
/// to the first-entry offset. The `eof` entry marks the table's end (and
/// eats one control byte per group when the count exceeds one).
fn tagx_block(index_record: &[u8]) -> Option<(usize, Vec<(u8, u8, u8, u8)>)> {
    // TAGX starts after the INDX header: the header's own length sits at
    // offset 4 (192 on every book this reader has seen), and its declared
    // TAGX offset at 180.
    let declared = u32(index_record, 180) as usize;
    let start = if index_record.len() > declared + 8
        && &index_record[declared..declared + 4] == b"TAGX"
    {
        declared
    } else {
        index_record[16..]
            .windows(4)
            .position(|w| w == b"TAGX")
            .map(|at| at + 16)?
    };
    let first_entry = u32(index_record, start + 4) as usize;
    let control_byte_count = u32(index_record, start + 8) as usize;
    if first_entry < 12 || control_byte_count == 0 || control_byte_count > 64 {
        return None;
    }
    let mut tags = Vec::new();
    let mut cursor = start + 12;
    while cursor + 4 <= index_record.len() && cursor < start + first_entry {
        tags.push((
            index_record[cursor],
            index_record[cursor + 1],
            index_record[cursor + 2],
            index_record[cursor + 3],
        ));
        cursor += 4;
    }
    if tags.is_empty() {
        return None;
    }
    Some((control_byte_count, tags))
}

/// One index entry's tag values: `[length byte][ident][control bytes]`,
/// then per TAGX tag the values its control-byte mask allows — every tag
/// reads the same control byte, its own mask selecting its slot.
///
/// When the masked value fills the whole mask and the mask has more than
/// one bit, a vwi follows giving the BYTE LENGTH of the value area (not a
/// count) — the multi-value tags like the fragment map's position pair use
/// that form.
fn entry_tag_values(
    entry: &[u8],
    control_byte_count: usize,
    tags: &[(u8, u8, u8, u8)],
) -> Option<Vec<(u8, Vec<u64>)>> {
    if entry.is_empty() {
        return None;
    }
    let ident_len = entry[0] as usize;
    let mut pos = 1 + ident_len;
    let mut control: &[u8] = entry.get(pos..pos + control_byte_count)?;
    pos += control.len();
    let mut values: Vec<(u8, Vec<u64>)> = Vec::new();
    for &(tag, numvals, mask, eof) in tags {
        if eof == 1 {
            control = &control[1.min(control.len())..];
            continue;
        }
        let Some(&byte) = control.first() else { break };
        let masked = byte & mask;
        if masked == 0 {
            continue;
        }
        let mut tag_values: Vec<u64> = Vec::new();
        if masked == mask && mask.count_ones() > 1 {
            let Some((byte_length, consumed)) = decint(entry, pos) else {
                break;
            };
            pos += consumed;
            let mut read = 0usize;
            while read < byte_length as usize {
                let Some((value, consumed)) = decint(entry, pos) else {
                    break;
                };
                pos += consumed;
                read += consumed;
                tag_values.push(value);
            }
        } else {
            let mut count = masked;
            let mut m = mask;
            while m & 1 == 0 {
                m >>= 1;
                count >>= 1;
            }
            for _ in 0..count as usize * numvals as usize {
                let Some((value, consumed)) = decint(entry, pos) else {
                    break;
                };
                pos += consumed;
                tag_values.push(value);
            }
        }
        values.push((tag, tag_values));
    }
    Some(values)
}

/// One TOC entry, when the index entry carries both a position (tag 1) and
/// a label (tag 3); the depth comes from tag 4 when it is there.
fn toc_entry(
    entry: &[u8],
    control_byte_count: usize,
    tags: &[(u8, u8, u8, u8)],
    strings: &HashMap<u64, String>,
) -> Option<TocEntry> {
    let values = entry_tag_values(entry, control_byte_count, tags)?;
    let position = values.iter().find(|(tag, _)| *tag == 1)?.1.first()?;
    let label_offset = values.iter().find(|(tag, _)| *tag == 3)?.1.first()?;
    let label = strings.get(label_offset)?;
    if label.is_empty() {
        return None;
    }
    let depth = values
        .iter()
        .find(|(tag, _)| *tag == 4)
        .and_then(|(_, v)| v.first())
        .copied()
        .unwrap_or(0) as usize;
    Some(TocEntry {
        label: label.clone(),
        offset: *position as usize,
        depth,
    })
}

/// The CNCX records' strings, keyed by their byte offset within the blob.
/// A CNCX record has no header — it IS the strings: `[vwi length][bytes]`,
/// back to back, from byte 0, each string keyed at its length byte (the
/// index entries' tag-3 values count from there — the real book's first
/// label sits at 0). Multi-record blobs address the later records 0x10000
/// up, the convention calibre reads.
fn cncx_strings(
    pdb: &PalmDatabase,
    first_record: usize,
    count: usize,
    codepage: u32,
) -> HashMap<u64, String> {
    let mut strings = HashMap::new();
    let mut record_base = 0u64;
    for index in first_record..first_record + count {
        let Some(record) = pdb.record(index) else { continue };
        let mut pos = 0usize;
        while pos < record.len() {
            let start = pos;
            let Some((length, consumed)) = decint(record, pos) else {
                break;
            };
            pos += consumed;
            if length == 0 {
                continue;
            }
            let end = pos + length as usize;
            if end > record.len() {
                break;
            }
            let bytes = &record[pos..end];
            let text = match codepage {
                65001 => String::from_utf8_lossy(bytes).into_owned(),
                1251 => super::bookdoc::decode_cp1251(bytes),
                1250 => super::codepages::decode_cp1250(bytes),
                _ => super::bookdoc::decode_cp1252(bytes),
            };
            if !text.is_empty() {
                strings.insert(record_base + start as u64, text);
            }
            pos = end;
        }
        record_base += 0x10000;
    }
    strings
}

/// A variable-width integer, forward: seven bits per byte, most significant
/// first, read until a byte with the top bit set — that byte's bits are
/// part of the value (calibre's `decint`, which the CNCX and the index
/// entries both use).
fn decint(data: &[u8], pos: usize) -> Option<(u64, usize)> {
    let mut value = 0u64;
    let mut consumed = 0usize;
    loop {
        let byte = *data.get(pos + consumed)?;
        value = (value << 7) | (byte & 0x7F) as u64;
        consumed += 1;
        if byte & 0x80 != 0 || consumed > 10 {
            return Some((value, consumed));
        }
    }
}

// ---------------------------------------------------------------------------
// Byte helpers
// ---------------------------------------------------------------------------

/// PalmDB/MOBI structure fields are stored BIG-endian (68k-era format).
fn u16(data: &[u8], offset: usize) -> u16 {
    if offset + 2 > data.len() {
        return 0;
    }
    ((data[offset] as u16) << 8) | data[offset + 1] as u16
}

fn u32(data: &[u8], offset: usize) -> u32 {
    if offset + 4 > data.len() {
        return 0;
    }
    ((data[offset] as u32) << 24)
        | ((data[offset + 1] as u32) << 16)
        | ((data[offset + 2] as u32) << 8)
        | data[offset + 3] as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // Fixture builders — the Palm Database and the MOBI header built
    // byte-wise, the way the Swift reference's tests do: a real PDB
    // (header, record list, FAT-free offsets) carrying genuine records, so
    // the parts a real file exercises (the record-offset list, the
    // trailing-entry table, EXTH inside record 0, the command-byte
    // encoding) are all covered without shipping megabyte fixtures.
    // ------------------------------------------------------------------

    fn be16(value: u16) -> [u8; 2] {
        value.to_be_bytes()
    }

    fn be32(value: u32) -> [u8; 4] {
        value.to_be_bytes()
    }

    /// A Palm Database with `records`, laid out back to back after the
    /// 78-byte header plus the record list. Records are contiguous bytes
    /// at their offset — NO per-record checksum (the format has none; a
    /// draft that stripped two bytes shifted every real book's records).
    fn make_pdb(records: &[&[u8]]) -> Vec<u8> {
        let list_size = records.len() * 8;
        let list_end = 78 + list_size;
        let mut header = vec![0u8; 78];
        header[60..64].copy_from_slice(b"BOOK");
        header[64..68].copy_from_slice(b"MOBI");
        header[76..78].copy_from_slice(&be16(records.len() as u16));

        let mut offsets = Vec::new();
        let mut payload = Vec::new();
        let mut cursor = list_end + 2; // +2 for the next-IDs list's terminating zero
        for record in records {
            offsets.push(cursor);
            payload.extend_from_slice(record);
            cursor += record.len();
        }
        let mut list = vec![0u8; list_size];
        for (index, offset) in offsets.iter().enumerate() {
            list[index * 8..index * 8 + 4].copy_from_slice(&be32(*offset as u32));
        }
        let mut out = header;
        out.extend_from_slice(&list);
        out.extend_from_slice(&[0u8; 2]); // the next-IDs list's terminating zero
        out.extend_from_slice(&payload);
        out
    }

    /// One EXTH entry: its type, its payload (the length covers the 8-byte
    /// type+length pair, the way real records declare it).
    fn exth_entry(kind: u32, payload: &[u8]) -> Vec<u8> {
        let mut entry = Vec::new();
        entry.extend_from_slice(&be32(kind));
        entry.extend_from_slice(&be32((8 + payload.len()) as u32));
        entry.extend_from_slice(payload);
        entry
    }

    fn exth_text_entry(kind: u32, text: &str) -> Vec<u8> {
        exth_entry(kind, text.as_bytes())
    }

    /// The header record's variable fields, defaulted the way a real
    /// calibre-produced MOBI6 book has them.
    struct Hdr {
        compression: u16,
        text_record_count: usize,
        encryption: u16,
        extra_flags: u16,
        codepage: u32,
        version: u32,
        first_image_index: Option<u32>,
        ncx_index: Option<u32>,
        exth: Vec<Vec<u8>>,
    }

    impl Default for Hdr {
        fn default() -> Self {
            Self {
                compression: 2,
                text_record_count: 1,
                encryption: 0,
                extra_flags: 0,
                codepage: 65001,
                version: 6,
                first_image_index: None,
                ncx_index: None,
                exth: Vec::new(),
            }
        }
    }

    /// A MOBI header record: the 16-byte PalmDOC part, the 232-byte MOBI
    /// header, then the EXTH. The header keeps its full 232 bytes — a real
    /// MOBI6 header is 232 and EXTH follows at 16 + 232 (trimming it moved
    /// the block and broke the reader, once upon a time).
    fn make_header_record(h: &Hdr, text_length: usize) -> Vec<u8> {
        let header_length = 232usize;
        let mut record = vec![0u8; 16 + header_length];
        record[0..2].copy_from_slice(&be16(h.compression));
        record[4..8].copy_from_slice(&be32(text_length as u32));
        record[8..10].copy_from_slice(&be16(h.text_record_count as u16));
        record[10..12].copy_from_slice(&be16(4096)); // record size
        record[12..14].copy_from_slice(&be16(h.encryption));
        record[16..20].copy_from_slice(b"MOBI");
        record[20..24].copy_from_slice(&be32(header_length as u32));
        record[24..28].copy_from_slice(&be32(2)); // type: a book
        record[28..32].copy_from_slice(&be32(h.codepage));
        record[0x68..0x6C].copy_from_slice(&be32(h.version));
        if let Some(first_image) = h.first_image_index {
            record[0x6C..0x70].copy_from_slice(&be32(first_image));
        }
        if header_length >= 0xE4 {
            record[0xF2..0xF4].copy_from_slice(&be16(h.extra_flags));
        }
        if let Some(ncx) = h.ncx_index {
            record[0xF4..0xF8].copy_from_slice(&be32(ncx));
        }
        if !h.exth.is_empty() {
            // The flag word at 0x80 tells a reader to look for the block.
            record[0x80..0x84].copy_from_slice(&be32(0x40));
            let exth_bytes: usize = h.exth.iter().map(|e| e.len()).sum();
            let mut exth = Vec::new();
            exth.extend_from_slice(b"EXTH");
            // The declared length includes the 8 bytes after the magic.
            exth.extend_from_slice(&be32((12 + exth_bytes) as u32));
            exth.extend_from_slice(&be32(h.exth.len() as u32));
            for entry in &h.exth {
                exth.extend_from_slice(entry);
            }
            record.extend_from_slice(&exth);
        }
        record
    }

    /// PalmDOC "compressed" bytes: everything as literal runs of up to 8 —
    /// valid input in every command-byte case, and the round-trip is exact.
    fn palmdoc_encode(text: &str) -> Vec<u8> {
        palmdoc_encode_bytes(text.as_bytes())
    }

    fn palmdoc_encode_bytes(bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut index = 0;
        while index < bytes.len() {
            let take = (8).min(bytes.len() - index);
            out.push(take as u8);
            out.extend_from_slice(&bytes[index..index + take]);
            index += take;
        }
        out
    }

    /// A forward variable-width integer: 7-bit groups, most significant
    /// first, the last byte carrying the terminator bit.
    fn vwi(value: u64) -> Vec<u8> {
        let mut out = vec![(value & 0x7F) as u8 | 0x80];
        let mut rest = value >> 7;
        while rest > 0 {
            out.insert(0, (rest & 0x7F) as u8);
            rest >>= 7;
        }
        out
    }

    /// One index entry: `[length byte][ident][control byte][vwi values]`,
    /// the values in TAGX order.
    fn make_index_entry(ident: &str, control: u8, values: &[u64]) -> Vec<u8> {
        let mut entry = vec![ident.len() as u8];
        entry.extend_from_slice(ident.as_bytes());
        entry.push(control);
        for value in values {
            entry.extend_from_slice(&vwi(*value));
        }
        entry
    }

    /// The NCX-style TAGX block: position (1), length (2), CNCX label (3),
    /// depth (4), and the eof marker.
    fn ncx_tagx() -> Vec<u8> {
        let mut tagx = Vec::new();
        tagx.extend_from_slice(b"TAGX");
        tagx.extend_from_slice(&be32(32)); // first entry offset: 12 + 5·4
        tagx.extend_from_slice(&be32(1)); // one control byte
        for (tag, numvals, mask, eof) in
            [(1u8, 1u8, 1u8, 0u8), (2, 1, 2, 0), (3, 1, 4, 0), (4, 1, 8, 0), (0, 0, 0, 1)]
        {
            tagx.extend_from_slice(&[tag, numvals, mask, eof]);
        }
        tagx
    }

    /// The index header record: 192 bytes of header (the count field says
    /// how many continuation records follow, ncncx how many CNCX records
    /// come after those), then the TAGX at 192.
    fn make_indx_header(ncncx: usize) -> Vec<u8> {
        let mut record = vec![0u8; 192];
        record[0..4].copy_from_slice(b"INDX");
        record[4..8].copy_from_slice(&be32(192));
        record[0x18..0x1C].copy_from_slice(&be32(1)); // one continuation
        record[0x1C..0x20].copy_from_slice(&be32(65001)); // encoding
        record[0x34..0x38].copy_from_slice(&be32(ncncx as u32));
        record[180..184].copy_from_slice(&be32(192)); // the TAGX offset
        record.extend_from_slice(&ncx_tagx());
        record
    }

    /// A continuation record: its own header (the count is its OWN entry
    /// count), the entries, then the IDXT offset table.
    fn make_indx_continuation(entries: &[Vec<u8>]) -> Vec<u8> {
        let entries_size: usize = entries.iter().map(|e| e.len()).sum();
        let idxt_at = 192 + entries_size;
        let mut record = vec![0u8; idxt_at + 4 + 2 * entries.len()];
        record[0..4].copy_from_slice(b"INDX");
        record[4..8].copy_from_slice(&be32(192));
        record[0x14..0x18].copy_from_slice(&be32(idxt_at as u32));
        record[0x18..0x1C].copy_from_slice(&be32(entries.len() as u32));
        let mut offsets = Vec::new();
        let mut at = 192;
        for entry in entries {
            offsets.push(at);
            record[at..at + entry.len()].copy_from_slice(entry);
            at += entry.len();
        }
        record[idxt_at..idxt_at + 4].copy_from_slice(b"IDXT");
        for (index, offset) in offsets.iter().enumerate() {
            let at = idxt_at + 4 + 2 * index;
            record[at..at + 2].copy_from_slice(&be16(*offset as u16));
        }
        record
    }

    /// A CNCX record — no header, just the strings: vwi length + bytes.
    fn make_cncx(strings: &[&str]) -> Vec<u8> {
        let mut record = Vec::new();
        for text in strings {
            record.extend_from_slice(&vwi(text.len() as u64));
            record.extend_from_slice(text.as_bytes());
        }
        record
    }

    /// The minimal-but-real HUFF/CDIC table pair from the huffcdic tests:
    /// every top byte a terminal 8-bit entry, 256 phrases of "Hi ".
    fn huff_cdic_records() -> (Vec<u8>, Vec<u8>) {
        let mut huff = Vec::new();
        huff.extend_from_slice(b"HUFF");
        huff.extend_from_slice(&be32(0x18));
        huff.extend_from_slice(&be32(24)); // off1
        huff.extend_from_slice(&be32(24 + 4 * 256)); // off2
        huff.extend_from_slice(&[0u8; 8]);
        for _ in 0..256 {
            huff.extend_from_slice(&be32(0xFF88)); // codelen 8, terminal
        }
        huff.extend_from_slice(&[0u8; 4 * 64]); // dict2

        let phrase = b"Hi ";
        let entries = 256usize;
        let mut cdic = Vec::new();
        cdic.extend_from_slice(b"CDIC");
        cdic.extend_from_slice(&be32(0x10));
        cdic.extend_from_slice(&be32(entries as u32)); // phrase count
        cdic.extend_from_slice(&be32(8)); // index bits
        // The offset table: cumulative byte offsets (from the table's own
        // start) to each entry's [length][payload] pair.
        let mut offset = 2 * entries;
        for _ in 0..entries {
            cdic.extend_from_slice(&be16(offset as u16));
            offset += 2 + phrase.len();
        }
        for _ in 0..entries {
            // length with bit 15 set: a plain entry
            cdic.extend_from_slice(&be16(0x8000u16 | phrase.len() as u16));
            cdic.extend_from_slice(phrase);
        }
        (huff, cdic)
    }

    fn jpeg(size: usize, fill: u8) -> Vec<u8> {
        let mut data = vec![0xFF, 0xD8, 0xFF, 0xE0];
        data.resize(size, fill);
        data
    }

    // ------------------------------------------------------------------
    // Decompression
    // ------------------------------------------------------------------

    #[test]
    fn literal_bytes_decode_themselves() {
        // '<' is 0x3C — a literal, NOT a flag byte with four back-references.
        // This single assertion is the whole reason the decoder exists:
        // every widely-copied flag-byte decoder fails exactly here.
        let out = decompress_palmdoc(b"<html><body>");
        assert_eq!(out, b"<html><body>".to_vec());
    }

    #[test]
    fn literal_run_command() {
        let mut input = vec![8u8];
        input.extend_from_slice(b"abcdefgh");
        input.push(3);
        input.extend_from_slice(b"ijk");
        assert_eq!(decompress_palmdoc(&input), b"abcdefghijk".to_vec());
    }

    #[test]
    fn space_plus_ascii_shorthand() {
        // 0xC0-0xFF is the byte-pair form: ' ' then (byte ^ 0x80).
        assert_eq!(decompress_palmdoc(&[0xC1, 0xE9]), b" A i".to_vec());
    }

    #[test]
    fn back_reference_repeats_earlier_bytes() {
        // 'a' literal, then distance 1, length 3 → "aaaa". value =
        // (distance << 3) | (length - 3) = 0x0008, high byte 0x80.
        let input = [0x61, 0x80, 0x08];
        assert_eq!(decompress_palmdoc(&input), b"aaaa".to_vec());
    }

    #[test]
    fn out_of_range_back_reference_is_skipped_not_clamped() {
        // A reference pointing before the start of the output emits
        // nothing — clamping would splice unrelated bytes into the prose.
        let input = [0x61, 0x80, 0x40];
        assert_eq!(decompress_palmdoc(&input), b"a".to_vec());
    }

    #[test]
    fn a_back_reference_may_cross_a_record_boundary() {
        // The PalmDOC path decompresses the JOINED records: the second
        // record's reference reaches back into the first record's output.
        // distance 3, length 3 → the "lo " before the boundary.
        let first = palmdoc_encode("hello ");
        let second = {
            let mut out = vec![0x80u8, 0x18];
            out.extend_from_slice(b"world");
            out
        };
        let mut joined = first;
        joined.extend_from_slice(&second);
        assert_eq!(decompress_palmdoc(&joined), b"hello lo world".to_vec());
    }

    // ------------------------------------------------------------------
    // Trailing entries
    // ------------------------------------------------------------------

    #[test]
    fn trailing_entry_bytes_are_stripped() {
        // Byte order: [text][bit-0 entry's payload byte][its count byte]
        // [bit-1 entry's two payload bytes][its size varint 0x82 = 2].
        // flags 0x3 strips 5, leaving "body".
        let mut record = b"body".to_vec();
        record.extend_from_slice(&[0x41, 0x81, 0x42, 0x43, 0x82]);
        let cut = trailing_entry_bytes(&record, 3);
        assert_eq!(cut, 5);
        assert_eq!(&record[..record.len() - cut], b"body");
    }

    #[test]
    fn trailing_bytes_match_the_reference_on_a_real_record_tail() {
        // The exact tail the real book's records carry — hex-dumped from
        // record 1 of Pocket Pediatrics. flags 0x3 strips 5: the bit-1
        // varint reads VALUE 4 backward from 0x84, then bit 0 adds 1 for
        // the 0x00 count byte. Identical to calibre on the same bytes.
        let mut record = b"x".to_vec();
        record.extend_from_slice(&[0x00, 0x00, 0x86, 0x88, 0x02, 0x84]);
        assert_eq!(trailing_entry_bytes(&record, 3), 5);
    }

    #[test]
    fn a_trailing_varint_carries_a_value_not_a_count() {
        let mut record = b"body".to_vec();
        record.extend_from_slice(&[0x88, 0x02]);
        // Backward walk: 0x02 (bitpos 0) then 0x88 (bitpos 7, terminator)
        // → value 8 | (2 << 7) = 1026 — NOT 2.
        assert_eq!(trailing_entry_bytes(&record, 2), 1026);
    }

    #[test]
    fn zero_extra_flags_means_no_stripping() {
        assert_eq!(trailing_entry_bytes(b"body", 0), 0);
    }

    #[test]
    fn the_multibyte_flag_adds_its_count_byte() {
        // flags 0x1: one byte, (byte & 0x3) + 1 — the multibyte overlap
        // region's size, read at the cut point.
        let mut record = palmdoc_encode("<p>x</p>");
        record.extend_from_slice(&[0x02]);
        assert_eq!(trailing_entry_bytes(&record, 1), 3);
    }

    // ------------------------------------------------------------------
    // Whole-book parse
    // ------------------------------------------------------------------

    #[test]
    fn parses_a_single_record_book() {
        let html = "<html><body><p>The first line of prose.</p></body></html>";
        let header = make_header_record(&Hdr::default(), html.len());
        let text = palmdoc_encode(html);
        let doc = parse(&make_pdb(&[&header, &text])).unwrap();
        assert_eq!(doc.chapters.len(), 1);
        assert!(doc.chapters[0].html.contains("The first line of prose."), "{}", doc.chapters[0].html);
        assert_eq!(doc.title, None);
    }

    #[test]
    fn headings_become_titled_chapters() {
        let html = "<html><body><h1>Chapter One</h1><p>Alpha text.</p>\
                    <h2>Chapter Two</h2><p>Beta text.</p></body></html>";
        let header = make_header_record(&Hdr::default(), html.len());
        let text = palmdoc_encode(html);
        let doc = parse(&make_pdb(&[&header, &text])).unwrap();
        let titles: Vec<_> = doc.chapters.iter().map(|c| c.title.clone()).collect();
        assert_eq!(titles, vec![Some("Chapter One".into()), Some("Chapter Two".into())]);
        assert!(doc.chapters[0].html.contains("Alpha text."));
        assert!(doc.chapters[1].html.contains("Beta text."));
        // The contents names the chapters in order.
        assert_eq!(doc.toc[0].label, "Chapter One");
        assert_eq!(doc.toc[1].chapter_index, 1);
    }

    #[test]
    fn deep_headings_stay_inside_their_chapter() {
        let html = "<html><body><h1>Part One</h1><p>Intro.</p>\
                    <h3>A subsection</h3><p>Body of the subsection.</p></body></html>";
        let header = make_header_record(&Hdr::default(), html.len());
        let text = palmdoc_encode(html);
        let doc = parse(&make_pdb(&[&header, &text])).unwrap();
        assert_eq!(doc.chapters.len(), 1);
        assert_eq!(doc.chapters[0].title.as_deref(), Some("Part One"));
        assert!(doc.chapters[0].html.contains("A subsection"));
        assert!(doc.chapters[0].html.contains("Body of the subsection."));
    }

    #[test]
    fn multi_record_text_concatenates() {
        let first = "<html><body><h1>One</h1><p>First half.";
        let second = "Second half.</p></body></html>";
        let hdr = Hdr {
            text_record_count: 2,
            ..Hdr::default()
        };
        let header = make_header_record(&hdr, first.len() + second.len());
        let a = palmdoc_encode(first);
        let b = palmdoc_encode(second);
        let doc = parse(&make_pdb(&[&header, &a, &b])).unwrap();
        assert_eq!(doc.chapters.len(), 1);
        assert!(doc.chapters[0].html.contains("First half."));
        assert!(doc.chapters[0].html.contains("Second half."));
    }

    #[test]
    fn exth_metadata_becomes_title_and_author() {
        let html = "<html><body><p>Body.</p></body></html>";
        let hdr = Hdr {
            exth: vec![exth_text_entry(503, "A Real Book"), exth_text_entry(100, "A Writer")],
            ..Hdr::default()
        };
        let header = make_header_record(&hdr, html.len());
        let text = palmdoc_encode(html);
        let doc = parse(&make_pdb(&[&header, &text])).unwrap();
        assert_eq!(doc.title.as_deref(), Some("A Real Book"));
        assert_eq!(doc.author.as_deref(), Some("A Writer"));
    }

    #[test]
    fn editorial_markers_are_stripped() {
        let html = "<html><body><p>[Illustration] The prose itself. [Pg 42]</p></body></html>";
        let header = make_header_record(&Hdr::default(), html.len());
        let text = palmdoc_encode(html);
        let doc = parse(&make_pdb(&[&header, &text])).unwrap();
        let html = &doc.chapters[0].html;
        assert!(html.contains("The prose itself."), "{html}");
        assert!(!html.contains("Illustration"), "{html}");
        assert!(!html.contains("Pg 42"), "{html}");
    }

    #[test]
    fn glyph_references_and_emphasis_marks_go() {
        assert_eq!(sanitize("a (cid:77) b"), "a b");
        assert_eq!(sanitize("so *bold* of you"), "so bold of you");
        // A word wrapped in underscores keeps them: the reference's
        // lookarounds only match a run with non-words on BOTH sides.
        assert_eq!(sanitize("so _bold_ of them"), "so _bold_ of them");
        // Words with underscores keep them; a four-run matches nowhere.
        assert_eq!(sanitize("snake_case and ____ four"), "snake_case and four");
    }

    #[test]
    fn script_and_style_bodies_are_never_read() {
        let html = "<html><head><style>p { color: red }</style></head>\
                    <body><p>Visible prose.</p></body></html>";
        let header = make_header_record(&Hdr::default(), html.len());
        let text = palmdoc_encode(html);
        let doc = parse(&make_pdb(&[&header, &text])).unwrap();
        let html = doc.chapters.iter().map(|c| c.html.as_str()).collect::<String>();
        assert!(html.contains("Visible prose."), "{html}");
        assert!(!html.contains("color"), "{html}");
    }

    #[test]
    fn a_pagebreak_separates_the_paragraphs() {
        let html = "<p>Before.</p><mbp:pagebreak/><p>After.</p>";
        let header = make_header_record(&Hdr::default(), html.len());
        let text = palmdoc_encode(html);
        let doc = parse(&make_pdb(&[&header, &text])).unwrap();
        let html = doc.chapters[0].html.as_str();
        assert!(html.contains("<p>Before.</p>"), "{html}");
        assert!(html.contains("<p>After.</p>"), "{html}");
    }

    #[test]
    fn a_cp1252_book_decodes() {
        let mut text = palmdoc_encode_bytes(b"<p>A \x93quote\x94</p>");
        let hdr = Hdr {
            codepage: 1252,
            ..Hdr::default()
        };
        let header = make_header_record(&hdr, text.len() - 4);
        let _ = &mut text;
        let doc = parse(&make_pdb(&[&header, &text])).unwrap();
        assert!(doc.chapters[0].html.contains("A \u{201C}quote\u{201D}"), "{}", doc.chapters[0].html);
    }

    // ------------------------------------------------------------------
    // Failures, honestly reported
    // ------------------------------------------------------------------

    #[test]
    fn a_drm_locked_book_says_so() {
        let html = "<html><body><p>x</p></body></html>";
        let hdr = Hdr {
            encryption: 2,
            ..Hdr::default()
        };
        let header = make_header_record(&hdr, html.len());
        let text = palmdoc_encode(html);
        let err = parse(&make_pdb(&[&header, &text])).unwrap_err();
        assert!(matches!(err, BookDocError::Unsupported(_)), "{err}");
        assert!(err.to_string().contains("DRM"), "{err}");
    }

    #[test]
    fn a_huff_cdic_book_without_tables_says_malformed_not_noise() {
        // 'DH' declared but the HUFF/CDIC records are missing — an honest
        // malformed error, never a decode of the bitstream as noise.
        let hdr = Hdr {
            compression: 17480,
            ..Hdr::default()
        };
        let header = make_header_record(&hdr, 10);
        let text = b"x".to_vec();
        let err = parse(&make_pdb(&[&header, &text])).unwrap_err();
        assert!(matches!(err, BookDocError::Malformed(_)), "{err}");
        assert!(err.to_string().contains("tables are missing"), "{err}");
    }

    #[test]
    fn a_non_book_file_is_rejected() {
        for data in [
            b"just some text".as_slice(),
            &[][..],
        ] {
            let err = parse(data).unwrap_err();
            assert!(err.to_string().contains("not a Mobipocket book"), "{err}");
        }
        // A ZIP named .mobi: the type/creator check catches it.
        let mut zip_like = vec![0x50u8; 200];
        zip_like[60..64].copy_from_slice(b"BOOK");
        zip_like[64..68].copy_from_slice(b"ZIPS");
        let err = parse(&zip_like).unwrap_err();
        assert!(err.to_string().contains("not a Mobipocket book"), "{err}");
    }

    #[test]
    fn a_pdb_of_another_flavor_refuses_with_the_same_message() {
        // The `.pdb` extension is shared with every Palm-era format; a
        // TealDoc/PalmDoc-only file reaches the parser through it and gets
        // the typed refusal, not a half-decode.
        let mut pdb = vec![0u8; 200];
        pdb[60..64].copy_from_slice(b"TEXt");
        pdb[64..68].copy_from_slice(b"REAd");
        let err = parse(&pdb).unwrap_err();
        assert!(err.to_string().contains("not a Mobipocket book"), "{err}");
    }

    #[test]
    fn a_header_claiming_more_records_than_exist_throws() {
        let hdr = Hdr {
            text_record_count: 5,
            ..Hdr::default()
        };
        let header = make_header_record(&hdr, 10);
        let text = b"x".to_vec();
        let err = parse(&make_pdb(&[&header, &text])).unwrap_err();
        assert!(err.to_string().contains("5 text record(s)"), "{err}");
    }

    #[test]
    fn an_unknown_compression_refuses() {
        let hdr = Hdr {
            compression: 3,
            ..Hdr::default()
        };
        let header = make_header_record(&hdr, 10);
        let text = b"x".to_vec();
        let err = parse(&make_pdb(&[&header, &text])).unwrap_err();
        assert!(err.to_string().contains("unknown compression 3"), "{err}");
    }

    #[test]
    fn looks_like_mobi_sniffs_the_type_and_creator() {
        let html = "<p>x</p>";
        let header = make_header_record(&Hdr::default(), html.len());
        let text = palmdoc_encode(html);
        let book = make_pdb(&[&header, &text]);
        assert!(looks_like_mobi(&book));
        assert!(looks_like_mobi(&book[..200.min(book.len())]));
        assert!(!looks_like_mobi(b"not a book"));
        assert!(!looks_like_mobi(&[]));
        // The ancient PRC creator reads too.
        let mut prc = book.clone();
        prc[64..68].copy_from_slice(b"REAd");
        assert!(looks_like_mobi(&prc));
    }

    // ------------------------------------------------------------------
    // Images
    // ------------------------------------------------------------------

    #[test]
    fn a_recindex_reference_becomes_a_pool_image() {
        let html = "<html><body><p>before <img recindex=\"1\" alt=\"a pic\"> after</p></body></html>";
        let header = make_header_record(&Hdr::default(), html.len());
        let text = palmdoc_encode(html);
        let image = jpeg(48, 0x11);
        let doc = parse(&make_pdb(&[&header, &text, &image])).unwrap();
        assert_eq!(doc.images.len(), 1, "{:?}", doc.images);
        assert_eq!(doc.images[0].name, "img/000000");
        assert_eq!(doc.images[0].mime, "image/jpeg");
        assert_eq!(doc.images[0].data, image);
        assert!(
            doc.chapters[0].html.contains("<img src=\"img/000000\" alt=\"a pic\">"),
            "{}",
            doc.chapters[0].html
        );
    }

    #[test]
    fn an_unresolvable_reference_loses_its_tag() {
        let html = "<html><body><p>before <img recindex=\"5\"> after</p></body></html>";
        let header = make_header_record(&Hdr::default(), html.len());
        let text = palmdoc_encode(html);
        let image = jpeg(48, 0x11);
        let doc = parse(&make_pdb(&[&header, &text, &image])).unwrap();
        assert!(doc.images.is_empty(), "{:?}", doc.images);
        assert!(!doc.chapters[0].html.contains("<img"), "{}", doc.chapters[0].html);
        assert!(doc.chapters[0].html.contains("after"), "{}", doc.chapters[0].html);
    }

    #[test]
    fn two_references_to_one_record_pool_one_image() {
        let html = "<html><body><img recindex=\"1\"><p>x</p><img recindex=\"1\"></body></html>";
        let header = make_header_record(&Hdr::default(), html.len());
        let text = palmdoc_encode(html);
        let image = jpeg(48, 0x11);
        let doc = parse(&make_pdb(&[&header, &text, &image])).unwrap();
        assert_eq!(doc.images.len(), 1, "{:?}", doc.images);
        assert_eq!(
            doc.chapters[0].html.matches("img/000000").count(),
            2,
            "{}",
            doc.chapters[0].html
        );
    }

    #[test]
    fn a_kindle_embed_reference_resolves_through_the_first_image_index() {
        // A standalone KF8 spells images `kindle:embed:NNNN`; the number
        // counts from the header's first image record.
        let html = "<html><body><p><img src=\"kindle:embed:0001?mime=image/jpg\" alt=\"\"></p></body></html>";
        let hdr = Hdr {
            version: 8,
            first_image_index: Some(2),
            ..Hdr::default()
        };
        let header = make_header_record(&hdr, html.len());
        let text = palmdoc_encode(html);
        let image = jpeg(48, 0x22);
        let doc = parse(&make_pdb(&[&header, &text, &image])).unwrap();
        assert_eq!(doc.images.len(), 1, "{:?}", doc.images);
        assert_eq!(doc.images[0].data, image);
        assert!(
            doc.chapters[0].html.contains("<img src=\"img/000000\">"),
            "{}",
            doc.chapters[0].html
        );
    }

    #[test]
    fn the_exth_cover_offset_wins_over_the_first_image() {
        // The real-book layout: the first image record is a small logo,
        // the cover sits at the EXTH 201 offset several records later.
        let html = "<html><body><p>x</p></body></html>";
        let hdr = Hdr {
            exth: vec![exth_entry(201, &be32(3))],
            ..Hdr::default()
        };
        let header = make_header_record(&hdr, html.len());
        let text = palmdoc_encode(html);
        let logo = jpeg(44, 0x11);
        let filler = vec![0u8; 12];
        let cover = jpeg(64, 0x22);
        let doc = parse(&make_pdb(&[&header, &text, &logo, &filler, &filler, &cover])).unwrap();
        assert_eq!(doc.cover.as_ref().unwrap().data, cover);
        assert_eq!(doc.cover.as_ref().unwrap().name, "img/cover");
    }

    #[test]
    fn the_exth_thumbnail_offset_is_the_fallback() {
        // 202, not 203: the reference read the fake-cover flag as an
        // offset. Here 201 is unset and 202 names the cover.
        let html = "<html><body><p>x</p></body></html>";
        let hdr = Hdr {
            exth: vec![exth_entry(202, &be32(1)), exth_entry(203, &be32(1))],
            ..Hdr::default()
        };
        let header = make_header_record(&hdr, html.len());
        let text = palmdoc_encode(html);
        let logo = jpeg(44, 0x11);
        let cover = jpeg(64, 0x22);
        let doc = parse(&make_pdb(&[&header, &text, &logo, &cover])).unwrap();
        assert_eq!(doc.cover.as_ref().unwrap().data, cover);
    }

    #[test]
    fn the_cover_falls_back_to_the_first_image_record() {
        let html = "<html><body><p>x</p></body></html>";
        let header = make_header_record(&Hdr::default(), html.len());
        let text = palmdoc_encode(html);
        let logo = jpeg(44, 0x11);
        let doc = parse(&make_pdb(&[&header, &text, &logo])).unwrap();
        assert_eq!(doc.cover.as_ref().unwrap().data, logo);
    }

    #[test]
    fn a_book_without_images_has_no_cover() {
        let html = "<html><body><p>x</p></body></html>";
        let header = make_header_record(&Hdr::default(), html.len());
        let text = palmdoc_encode(html);
        let doc = parse(&make_pdb(&[&header, &text])).unwrap();
        assert_eq!(doc.cover, None);
    }

    // ------------------------------------------------------------------
    // KF8 flow chapters
    // ------------------------------------------------------------------

    /// The KF8 rawML shape, distilled from a real store book: each flow is
    /// a complete `<html>…</html>` document whose section content sits
    /// AFTER the flow's close, one flow per chapter.
    const KF8_RAWML: &str = concat!(
        "<html><head><title>t</title></head><body></body></html>",
        "<section epub:type=\"titlepage\"><p>Title page art</p></section>",
        "<html><head><title>t</title></head><body></body></html>",
        "<section epub:type=\"chapter\"><h1>DEVELOPMENTAL MILESTONES</h1>",
        "<p>Chapter body.</p><h2>A subsection</h2><p>Still this chapter.</p></section>",
        "<html><head><title>t</title></head><body></body></html>",
        "<section epub:type=\"chapter\"><h1>TRAUMA EXAM</h1><p>Next body.</p></section>",
    );

    #[test]
    fn kf8_flows_split_at_document_boundaries_not_every_heading() {
        // The device report: a reference `.azw3` whose 165 h1s and 265 h2s
        // all opened "chapters". The flow boundary is the book's own
        // structure: three flows here, three chapters — the h2 stays
        // inside.
        let hdr = Hdr {
            version: 8,
            ..Hdr::default()
        };
        let header = make_header_record(&hdr, KF8_RAWML.len());
        let text = palmdoc_encode(KF8_RAWML);
        let doc = parse(&make_pdb(&[&header, &text])).unwrap();
        assert_eq!(doc.chapters.len(), 3, "{:?}", doc.chapters);
        // Flow 1 has no h1: its epub:type becomes the title.
        assert_eq!(doc.chapters[0].title.as_deref(), Some("Titlepage"));
        assert_eq!(
            doc.chapters[1].title.as_deref(),
            Some("DEVELOPMENTAL MILESTONES")
        );
        assert!(doc.chapters[1].html.contains("Still this chapter."), "{}", doc.chapters[1].html);
        // The h2 stays inside as prose, not a new chapter.
        assert!(doc.chapters[1].html.contains("<p>A subsection</p>"));
        assert_eq!(doc.chapters[2].title.as_deref(), Some("TRAUMA EXAM"));
    }

    #[test]
    fn kf8_flow_chapters_carry_no_html_wrapper() {
        // A flow's <html>…</html> must NOT ride into the chapter XHTML —
        // a nested one is where the reference's generated EPUB stopped
        // rendering (the "error then truncation" report on real KF8
        // books). The rebuilt body cannot carry one.
        let hdr = Hdr {
            version: 8,
            ..Hdr::default()
        };
        let header = make_header_record(&hdr, KF8_RAWML.len());
        let text = palmdoc_encode(KF8_RAWML);
        let doc = parse(&make_pdb(&[&header, &text])).unwrap();
        for chapter in &doc.chapters {
            assert!(!chapter.html.to_lowercase().contains("<html"), "{}", chapter.html);
            assert!(!chapter.html.to_lowercase().contains("</html"), "{}", chapter.html);
            assert!(chapter.html.contains("art") || chapter.html.contains("body"), "{}", chapter.html);
        }
    }

    #[test]
    fn kf8_repeated_title_only_appears_once() {
        // The real-book duplicate (the user's report): the chapter's h1
        // AND a re-stated label with the same text — the reader showed
        // "Chapter 3—Repentance" twice.
        let html = concat!(
            "<html><head><title>t</title></head><body></body></html>",
            "<section><h1>DEVELOPMENTAL MILESTONES</h1><p>Body one.</p></section>",
            "<html><head><title>t</title></head><body></body></html>",
            "<section><h1>DEVELOPMENTAL MILESTONES</h1></section>",
            "<html><head><title>t</title></head><body></body></html>",
            "<section><h1>TRAUMA EXAM</h1><p>Body two.</p></section>",
        );
        let hdr = Hdr {
            version: 8,
            ..Hdr::default()
        };
        let header = make_header_record(&hdr, html.len());
        let text = palmdoc_encode(html);
        let doc = parse(&make_pdb(&[&header, &text])).unwrap();
        let titles: Vec<_> = doc.chapters.iter().flat_map(|c| c.title.iter()).collect();
        assert_eq!(
            titles,
            vec![&"DEVELOPMENTAL MILESTONES".to_string(), &"TRAUMA EXAM".to_string()]
        );
    }

    #[test]
    fn kf8_title_echo_inside_the_flow_is_dropped() {
        // The other real-book shape: the title rides INSIDE one flow as an
        // h1 and again as a plain label paragraph right under it.
        let html = concat!(
            "<html><head><title>t</title></head><body></body></html>",
            "<section epub:type=\"chapter\"><h1>Chapter 3—Repentance</h1>",
            "<div><p>Chapter 3—Repentance</p></div>",
            "<p>How shall a man be just with God?</p></section>",
            "<html><head><title>t</title></head><body></body></html>",
            "<section epub:type=\"chapter\"><h1>Chapter 4—Confession</h1><p>Body.</p></section>",
        );
        let hdr = Hdr {
            version: 8,
            ..Hdr::default()
        };
        let header = make_header_record(&hdr, html.len());
        let text = palmdoc_encode(html);
        let doc = parse(&make_pdb(&[&header, &text])).unwrap();
        assert_eq!(doc.chapters.len(), 2);
        assert_eq!(doc.chapters[0].title.as_deref(), Some("Chapter 3—Repentance"));
        let first = &doc.chapters[0].html;
        assert!(!first.contains("<p>Chapter 3—Repentance</p>"), "{first}");
        assert!(first.contains("How shall a man be just with God?"), "{first}");
    }

    #[test]
    fn kf8_requires_two_flows_falls_back_to_headings() {
        // One stray <html>…</html> inside a MOBI6-shaped document is not a
        // KF8 structure — the heading path must run, not a one-chapter cut.
        // (A version-6 header, so the heading path applies regardless.)
        let html = "<h1>One</h1><p>First.</p>\
                    <html><head><title>t</title></head><body></body></html>\
                    <h1>Two</h1><p>Second.</p>";
        let header = make_header_record(&Hdr::default(), html.len());
        let text = palmdoc_encode(html);
        let doc = parse(&make_pdb(&[&header, &text])).unwrap();
        let titles: Vec<_> = doc.chapters.iter().flat_map(|c| c.title.iter()).collect();
        assert_eq!(titles, vec![&"One".to_string(), &"Two".to_string()]);
    }

    // ------------------------------------------------------------------
    // Huff/CDIC
    // ------------------------------------------------------------------

    #[test]
    fn a_huff_cdic_book_decodes_end_to_end() {
        let (huff, cdic) = huff_cdic_records();
        let hdr = Hdr {
            compression: 17480,
            ..Hdr::default()
        };
        // textLength 8 = three codes of "Hi " each: "Hi Hi Hi ".
        let header = make_header_record(&hdr, 8);
        let payload = [0x00u8, 0x00, 0x00];
        let doc = parse(&make_pdb(&[&header, &payload, &huff, &cdic])).unwrap();
        assert_eq!(doc.chapters.len(), 1);
        assert!(doc.chapters[0].html.contains("Hi Hi Hi"), "{}", doc.chapters[0].html);
    }

    // ------------------------------------------------------------------
    // The book's own contents (CNCX)
    // ------------------------------------------------------------------

    /// A book whose index names two chapters: the publisher's labels win
    /// and the cuts land on the index's byte offsets.
    fn toc_fixture(entry_values: &[(u64, u64, u64)]) -> Vec<u8> {
        let html = "<html><body><h1>One</h1><p>Alpha text.</p><h2>Two</h2><p>Beta text.</p></body></html>";
        let h1_at = html.find("<h1").unwrap() as u64;
        let cncx = make_cncx(&["Chapter One", "Chapter Two"]);
        let entries: Vec<Vec<u8>> = entry_values
            .iter()
            .map(|(base, one, two)| {
                // control 0x0D: tags 1 (position), 3 (CNCX), 4 (depth) —
                // tag 2's mask is clear.
                let position = h1_at + base;
                let label = if *one == 1 { 0 } else { 12 };
                make_index_entry(
                    "000",
                    0x0D,
                    &[position + two, label, 0],
                )
            })
            .collect();
        let indx = make_indx_header(1);
        let continuation = make_indx_continuation(&entries);
        let hdr = Hdr {
            ncx_index: Some(2),
            ..Hdr::default()
        };
        let header = make_header_record(&hdr, html.len());
        let text = palmdoc_encode(html);
        make_pdb(&[&header, &text, &indx, &continuation, &cncx])
    }

    #[test]
    fn the_publishers_contents_cut_the_chapters() {
        // Two entries: one at the h1, one at the h2.
        let pdb = toc_fixture(&[(0, 1, 0), (html_h2_delta(), 0, 0)]);
        let doc = parse(&pdb).unwrap();
        assert_eq!(doc.chapters.len(), 2, "{:?}", doc.chapters);
        // The publisher's label wins: it is what the contents page says.
        assert_eq!(doc.chapters[0].title.as_deref(), Some("Chapter One"));
        assert!(doc.chapters[0].html.contains("Alpha text."), "{}", doc.chapters[0].html);
        assert!(!doc.chapters[0].html.contains("Beta"), "{}", doc.chapters[0].html);
        assert_eq!(doc.chapters[1].title.as_deref(), Some("Chapter Two"));
        assert!(doc.chapters[1].html.contains("Beta text."), "{}", doc.chapters[1].html);
        // The contents rows carry the labels and the chapter order.
        assert_eq!(doc.toc.len(), 2);
        assert_eq!(doc.toc[0].label, "Chapter One");
        assert_eq!(doc.toc[0].chapter_index, 0);
        assert_eq!(doc.toc[1].label, "Chapter Two");
        assert_eq!(doc.toc[1].chapter_index, 1);
    }

    /// The byte distance from the h1 to the h2 in the fixture's text.
    fn html_h2_delta() -> u64 {
        let html = "<html><body><h1>One</h1><p>Alpha text.</p><h2>Two</h2><p>Beta text.</p></body></html>";
        (html.find("<h2").unwrap() - html.find("<h1").unwrap()) as u64
    }

    #[test]
    fn a_toc_offset_inside_a_tag_snaps_forward() {
        // The second entry's offset lands INSIDE the `<h2 …` open tag;
        // the cut snaps to the next tag boundary and the chapter still
        // reads.
        let pdb = toc_fixture(&[(0, 1, 0), (html_h2_delta() + 2, 0, 0)]);
        let doc = parse(&pdb).unwrap();
        assert_eq!(doc.chapters.len(), 2, "{:?}", doc.chapters);
        assert_eq!(doc.chapters[1].title.as_deref(), Some("Chapter Two"));
        assert!(doc.chapters[1].html.contains("Beta text."), "{}", doc.chapters[1].html);
    }

    #[test]
    fn a_one_entry_toc_falls_back_to_the_headings() {
        // An index with a single placeable entry cannot cut chapters; the
        // heading path takes over and the chapters keep their own titles.
        let pdb = toc_fixture(&[(0, 1, 0)]);
        let doc = parse(&pdb).unwrap();
        let titles: Vec<_> = doc.chapters.iter().flat_map(|c| c.title.iter()).collect();
        assert_eq!(titles, vec![&"One".to_string(), &"Two".to_string()]);
    }

    #[test]
    fn a_toc_entry_without_a_label_is_dropped() {
        // The label offset names nothing in the CNCX — the entry cannot
        // anchor a chapter and the heading path runs.
        let html = "<html><body><h1>One</h1><p>Alpha text.</p><h2>Two</h2><p>Beta text.</p></body></html>";
        let h1_at = html.find("<h1").unwrap() as u64;
        let h2_at = html.find("<h2").unwrap() as u64;
        let cncx = make_cncx(&["Chapter One"]);
        let entries = vec![
            make_index_entry("000", 0x0D, &[h1_at, 0, 0]),
            make_index_entry("001", 0x0D, &[h2_at, 40, 0]), // offset 40: no string
        ];
        let indx = make_indx_header(1);
        let continuation = make_indx_continuation(&entries);
        let hdr = Hdr {
            ncx_index: Some(2),
            ..Hdr::default()
        };
        let header = make_header_record(&hdr, html.len());
        let text = palmdoc_encode(html);
        let doc = parse(&make_pdb(&[&header, &text, &indx, &continuation, &cncx])).unwrap();
        let titles: Vec<_> = doc.chapters.iter().flat_map(|c| c.title.iter()).collect();
        assert_eq!(titles, vec![&"One".to_string(), &"Two".to_string()]);
    }

    #[test]
    fn a_toc_offset_names_a_depth() {
        // The index's nesting depth rides to the manifest's contents rows.
        let html = "<html><body><h1>One</h1><p>Alpha text.</p><h2>Two</h2><p>Beta text.</p></body></html>";
        let h1_at = html.find("<h1").unwrap() as u64;
        let h2_at = html.find("<h2").unwrap() as u64;
        let cncx = make_cncx(&["Chapter One", "Section One"]);
        let entries = vec![
            make_index_entry("000", 0x0D, &[h1_at, 0, 0]),
            make_index_entry("001", 0x0D, &[h2_at, 12, 1]),
        ];
        let indx = make_indx_header(1);
        let continuation = make_indx_continuation(&entries);
        let hdr = Hdr {
            ncx_index: Some(2),
            ..Hdr::default()
        };
        let header = make_header_record(&hdr, html.len());
        let text = palmdoc_encode(html);
        let doc = parse(&make_pdb(&[&header, &text, &indx, &continuation, &cncx])).unwrap();
        assert_eq!(doc.toc[0].depth, 0);
        assert_eq!(doc.toc[1].depth, 1);
    }

    // ------------------------------------------------------------------
    // Snapping, directly
    // ------------------------------------------------------------------

    #[test]
    fn snap_offset_walks_to_a_tag_boundary() {
        let bytes = b"<p>text</p> and <b>more</b>";
        assert_eq!(snap_offset(0, bytes), Some(0)); // already at '<'
        assert_eq!(snap_offset(1, bytes), Some(7)); // inside the <p>: to </p>
        assert_eq!(snap_offset(13, bytes), Some(16)); // to the <b>
        assert_eq!(snap_offset(bytes.len(), bytes), None);
        assert_eq!(snap_offset(bytes.len() + 5, bytes), None);
        // An offset already at a '<' snaps nowhere — the reference's
        // contract, stray or not (a cut inside real text walks forward).
        let stray = b"two < things <p>x</p>";
        assert_eq!(snap_offset(4, stray), Some(4));
        assert_eq!(snap_offset(8, stray), Some(13));
    }

    // ------------------------------------------------------------------
    // The index entry walk, directly
    // ------------------------------------------------------------------

    #[test]
    fn an_index_entry_decodes_its_masked_values() {
        // The real book's control-byte shape: 0x8f = tags 1, 2, 3, 4 and
        // the pair tag all present; the pair tag (numvals 2) reads two
        // vwis for one count.
        let tags = [(1u8, 1u8, 1u8, 0u8), (2, 1, 2, 0), (3, 1, 4, 0), (6, 2, 8, 0), (0, 0, 0, 1)];
        let entry = {
            let mut e = make_index_entry("000", 0x8F, &[]);
            e.extend_from_slice(&vwi(363));
            e.extend_from_slice(&vwi(2035));
            e.extend_from_slice(&vwi(0));
            e.extend_from_slice(&vwi(0));
            e.extend_from_slice(&vwi(1588));
            e
        };
        let values = entry_tag_values(&entry, 1, &tags).unwrap();
        let value = |tag: u8| values.iter().find(|(t, _)| *t == tag).map(|(_, v)| v.clone());
        assert_eq!(value(1), Some(vec![363]));
        assert_eq!(value(2), Some(vec![2035]));
        assert_eq!(value(3), Some(vec![0]));
        assert_eq!(value(6), Some(vec![0, 1588]));
    }

    #[test]
    fn the_byte_count_form_reads_its_own_vwis() {
        // When the masked value fills a multi-bit mask, a vwi gives the
        // BYTE length of the value area, and that many bytes' vwis follow.
        let tags = [(6u8, 2u8, 0xC0u8, 0u8), (0, 0, 0, 1)];
        let mut entry = make_index_entry("00", 0xC0, &[]);
        // Two vwis, 3 bytes total (1 + 2).
        entry.extend_from_slice(&vwi(3));
        entry.extend_from_slice(&vwi(7));
        entry.extend_from_slice(&vwi(300));
        let values = entry_tag_values(&entry, 1, &tags).unwrap();
        assert_eq!(values[0].1, vec![7, 300]);
    }

    #[test]
    fn a_cncx_record_is_its_strings() {
        // No header: the record IS [vwi length][bytes] from byte 0, the
        // shape both real books on this box carry. Each string is keyed at
        // its length byte — the first label at offset 0, the way the real
        // book's index entries name them.
        let pdb_data = make_pdb(&[&make_cncx(&["Copyright", "Table of Contents"])]);
        let pdb = PalmDatabase::parse(&pdb_data).unwrap();
        let strings = cncx_strings(&pdb, 0, 1, 65001);
        assert_eq!(strings.get(&0).map(String::as_str), Some("Copyright"));
        // "Copyright" = vwi(9) + 9 bytes → the next length byte at 10.
        assert_eq!(strings.get(&10).map(String::as_str), Some("Table of Contents"));
        assert_eq!(strings.get(&11), None);
    }

}
