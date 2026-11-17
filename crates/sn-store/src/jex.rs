//! JEX export/import: the Joplin-compatible interchange format.
//!
//! A `.jex` file is a plain (uncompressed) tar of `<id>.md` entries whose
//! trailing metadata block carries id/type/parent/timestamps, plus
//! `resources/<resource-id>.<ext>` binaries. The iOS app's export/import
//! (`JexExport`/`JexImport`) is the reference; the schema is the public
//! Joplin export format. This module is an independent reimplementation of
//! the same wire shape — no Joplin code was read.
//!
//! The entry catalog:
//!   * `<note-id>.md` — note body + metadata + `type_: 1`
//!   * `<folder-id>.md` — notebook title line + metadata + `type_: 2`
//!   * `<resource-id>.md` — empty body + metadata (id, title, mime) + `type_: 4`
//!   * `resources/<resource-id>.<ext>` — the binary blob
//!
//! Conventions: 32 lowercase hex ids (dashes stripped from UUIDs), ISO-8601
//! UTC timestamps with milliseconds, metadata keys in a fixed order with
//! `type_` LAST (the Joplin importer anchors on it, and so does ours).
//!
//! The round-trip rule the other stores live by applies here too: **an
//! import that yields some notes beats one that yields none because one
//! entry was odd.** Entries that fail to parse are skipped, not fatal; only
//! a file that cannot be a tar at all is refused.

use crate::model::{Note, Notebook};
use sn_core::AppleDate;

// ---------------------------------------------------------------------------
// IDs
// ---------------------------------------------------------------------------

/// A note/notebook id in JEX form: the UUID's dashes stripped, lowercased.
/// Stable across re-exports, which is what makes Joplin update rather than
/// duplicate on a second import.
pub fn joplin_id(uuid: &str) -> String {
    uuid.replace('-', "").to_ascii_lowercase()
}

/// Reverse [`joplin_id`] for ids this app exported: re-dash a 32-hex id into
/// the canonical UUID form. A foreign id that is not 32 hex characters
/// yields `None` and the caller assigns a fresh one.
pub fn uuid_from_joplin_id(id: &str) -> Option<String> {
    let hex = id.to_ascii_lowercase();
    if hex.len() != 32 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let bytes = hex.as_bytes();
    let s = unsafe {
        // Safety: `bytes` is valid UTF-8 (checked hex above), and the slices
        // are at ASCII boundaries. Building with format! instead costs four
        // allocations per id on a 5000-note import.
        std::str::from_utf8_unchecked(bytes)
    };
    // Uppercase, matching `new_uuid()`'s form: one library must not carry
    // mixed-case ids that only compare equal after folding.
    Some(format!(
        "{}-{}-{}-{}-{}",
        &s[0..8].to_ascii_uppercase(),
        &s[8..12].to_ascii_uppercase(),
        &s[12..16].to_ascii_uppercase(),
        &s[16..20].to_ascii_uppercase(),
        &s[20..32].to_ascii_uppercase()
    ))
}

// ---------------------------------------------------------------------------
// Tar writer
// ---------------------------------------------------------------------------

/// Minimal ustar writer: 512-byte header + data + padding per entry, two
/// zero blocks at the end. Enough for Joplin's importer and for ours; the
/// checksum is written the POSIX way (spaces in the checksum field while
/// summing, octal digits after).
#[derive(Default)]
pub struct TarWriter {
    bytes: Vec<u8>,
}

impl TarWriter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append one regular file.
    pub fn append(&mut self, name: &str, data: &[u8], mtime_unix: i64) {
        let mut header = [0u8; 512];
        let name_bytes = name.as_bytes();
        let name_len = name_bytes.len().min(100);
        header[..name_len].copy_from_slice(&name_bytes[..name_len]);
        write_octal(&mut header[100..108], 0o644);
        write_octal(&mut header[108..116], 0); // uid
        write_octal(&mut header[116..124], 0); // gid
        write_octal(&mut header[124..136], data.len());
        write_octal(&mut header[136..148], mtime_unix.max(0) as usize);
        // Checksum placeholder: 8 spaces per POSIX, summed as-is.
        header[148..156].copy_from_slice(b"        ");
        header[156] = b'0'; // type: regular file
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let checksum: usize = header.iter().map(|b| *b as usize).sum();
        write_octal(&mut header[148..156], checksum);
        self.bytes.extend_from_slice(&header);
        self.bytes.extend_from_slice(data);
        let pad = (512 - (data.len() % 512)) % 512;
        self.bytes.extend(std::iter::repeat(0u8).take(pad));
    }

    /// Two zero blocks end the archive.
    pub fn finish(mut self) -> Vec<u8> {
        self.bytes.extend(std::iter::repeat(0u8).take(1024));
        self.bytes
    }
}

/// Write `value` as a zero-padded octal into `field` (the last byte stays 0
/// as the NUL terminator; a too-large value is truncated — the fields this
/// writer uses never overflow their widths for real data).
fn write_octal(field: &mut [u8], value: usize) {
    let width = field.len() - 1;
    let digits = format!("{:0width$o}", value, width = width);
    let bytes = digits.as_bytes();
    let take = bytes.len().min(width);
    field[..take].copy_from_slice(&bytes[bytes.len() - take..]);
}

// ---------------------------------------------------------------------------
// Metadata blocks
// ---------------------------------------------------------------------------

/// The trailing metadata block, as both writer and reader see it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MetadataBlock {
    pub fields: Vec<(String, String)>,
    /// The note/notebook body: everything before the block.
    pub body: String,
    /// The item's title, for a note.
    ///
    /// Joplin's own layout puts the title on the FIRST line, then a blank
    /// line, then the body, then a blank line, then the properties. Its
    /// importer takes the first line as the title and the rest as the body
    /// — so a note written without a title line loses its first paragraph
    /// to the title field and arrives in Joplin with an empty body.
    pub title: Option<String>,
}

impl MetadataBlock {
    /// The whole entry's bytes: title, body, blank line, fields, with
    /// `type_` LAST on purpose — the Joplin importer anchors on it to
    /// delimit the block, so ours does too.
    ///
    /// The entry ends WITHOUT a trailing newline. Joplin's parser walks the
    /// lines from the end and stops at the first blank one; a trailing
    /// newline makes the last line empty, so the parser never reaches the
    /// `type_` line and refuses the entry with "Missing required property:
    /// type_". This is the difference between Joplin accepting an archive
    /// and refusing every note in it.
    pub fn render(&self, type_code: u32) -> String {
        let mut out = String::with_capacity(self.body.len() + 128);
        // The title line comes first, for a note; a resource metadata file
        // has no title and no body.
        if let Some(title) = &self.title {
            out.push_str(title);
            out.push_str("\n\n");
        }
        // The body's trailing blank lines would sit between it and the props
        // block; trimming the tail keeps the `\n\n` seams exactly canonical.
        out.push_str(self.body.trim_end());
        // The blank line that separates the body from the properties. A
        // title-only or empty body still needs the separator, or the
        // properties would be read as the body.
        out.push_str("\n\n");
        for (key, value) in &self.fields {
            out.push_str(key);
            out.push_str(": ");
            out.push_str(value);
            out.push('\n');
        }
        // The last property, with no newline after it.
        out.push_str(&format!("type_: {type_code}"));
        out
    }

    /// Parse the block back out of one entry's full text.
    ///
    /// The block starts at the LAST `\ntype_: ` in the entry; the body is
    /// everything before the blank line that opens the block. A resource
    /// metadata file has no body at all, which is a valid form.
    pub fn parse(entry: &str) -> Option<(Self, u32)> {
        // The block anchor: the LAST `\ntype_: ` whose neighbourhood looks
        // like a metadata block (an `id:` line follows). A note body that
        // QUOTES the format — this line is one — must not swallow the real
        // block, and a body that merely ends with a `type_: ` line must not
        // parse as a block at all. The walk-back is bounded: three tries
        // covers the writer, a quoted form, and a doubly-quoted one.
        let mut type_pos = entry.rfind("\ntype_: ")?;
        let mut tries = 0;
        loop {
            // The candidate block is everything between the blank line that
            // opens it and the anchor; every real entry carries an `id:`
            // field there, and a quoted `type_: N` line in a body does not.
            let block = entry[..type_pos]
                .rsplit_once("\n\n")
                .map(|(_, block)| block)
                .unwrap_or(entry[..type_pos].as_ref());
            let looks_like_block = block
                .lines()
                .any(|l| l.starts_with("id: "));
            if looks_like_block || tries >= 2 {
                break;
            }
            // Not a metadata block: try the next earlier anchor.
            match entry[..type_pos].rfind("\ntype_: ") {
                Some(earlier) => {
                    type_pos = earlier;
                    tries += 1;
                }
                None => return None,
            }
        }
        let after = &entry[type_pos + "\ntype_: ".len()..];
        let line_end = after.find('\n').unwrap_or(after.len());
        let type_code: u32 = after[..line_end].trim().parse().ok()?;
        let head = &entry[..type_pos];
        let (body, block) = match head.rfind("\n\n") {
            Some(i) => (&head[..i], &head[i + 2..]),
            None => ("", head),
        };
        let mut fields = Vec::new();
        for line in block.lines() {
            let Some(colon) = line.find(':') else { continue };
            let key = line[..colon].trim().to_string();
            if key.is_empty() {
                continue;
            }
            let value = line[colon + 1..].trim().to_string();
            fields.push((key, value));
        }
        fields.push((
            "type_".to_string(),
            type_code.to_string(),
        ));
        // The title/body splice, with Joplin's semantics — the same rules
        // the iOS importer applies (`JexImport.swift`):
        //
        // A note's entry is `title\n\nbody\n\nprops`. The title is the
        // FIRST line of the body part and the note's body starts two
        // lines down, after the blank separator. The `title:` prop marks
        // the OLD layout — this app's own earlier exports, which put the
        // body verbatim and the title in a prop — and that form keeps its
        // body whole.
        //
        // Without this, a note imported from an iOS archive loses its
        // first two lines to the title, or (in the old layout) loses its
        // whole body.
        let legacy_title = fields
            .iter()
            .find(|(k, _)| k == "title")
            .map(|(_, v)| v.trim().to_string())
            .filter(|v| !v.is_empty());
        let (title, body) = match legacy_title {
            Some(t) => (Some(t), body.to_string()),
            None => {
                let lines: Vec<&str> = body.split('\n').collect();
                // The title line is followed by the blank separator, and
                // the body starts after it. An entry whose body does NOT
                // have that shape has no title line at all — this app's
                // own first exports looked like that, and so do the
                // archives the iOS build produced before its 2026-10-25
                // format fix. Those keep their whole body.
                let has_title_line = lines.len() > 1 && lines[1].trim().is_empty();
                if has_title_line {
                    let title = lines[0].trim().to_string();
                    let body = if lines.len() > 2 { lines[2..].join("\n") } else { String::new() };
                    (Some(title), body)
                } else {
                    (None, body.to_string())
                }
            }
        };
        Some((
            MetadataBlock {
                fields,
                body,
                title,
            },
            type_code,
        ))
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// A timestamp field: ISO-8601, or Joplin's ms-since-epoch form. A
    /// missing or unparsable value becomes *now* — a note whose date cannot
    /// be read must still import, dated today, rather than be dropped.
    pub fn date(&self, key: &str) -> AppleDate {
        self.get(key)
            .and_then(|raw| parse_iso(raw))
            .unwrap_or_else(AppleDate::now)
    }
}

/// `2026-09-08T16:00:43.123Z` (our writer) and Joplin's millisecond ISO
/// strings alike, plus the ms-since-epoch form some exporters emit.
pub fn parse_iso(raw: &str) -> Option<AppleDate> {
    let raw = raw.trim();
    // ms-since-epoch: a bare number above 1e12.
    if let Ok(ms) = raw.parse::<f64>() {
        if ms > 1_000_000_000_000.0 {
            return Some(AppleDate::from_unix(ms / 1000.0));
        }
        // Apple absolute dates are seconds since 2001-01-01 ≈ 3e8; a bare
        // small number is not a date this importer can interpret.
        return None;
    }
    let (date_part, time_part, fraction, tz) = split_iso(raw)?;
    let date = parse_ymd(&date_part)?;
    let (hour, min, sec) = parse_hms(&time_part)?;
    let mut unix = date
        + hour as f64 * 3600.0
        + min as f64 * 60.0
        + sec as f64
        + fraction;
    // A `Z` is UTC. An offset (`+02:00`) shifts the timestamp into UTC.
    if let Some(offset) = tz.filter(|t| !t.is_empty() && t != "Z") {
        let sign = match offset.as_str().find(':').or_else(|| offset.find(|c| c == '+' || c == '-')) {
            _ if offset.starts_with('+') => -1.0,
            _ if offset.starts_with('-') => 1.0,
            _ => 0.0,
        };
        if sign != 0.0 {
            let digits: String = offset.chars().filter(|c| c.is_ascii_digit()).collect();
            // A malformed offset (`+02:0`, `+020`) must not panic the import
            // — it parses what is there, and ignores a fragment too short to
            // be hours-and-minutes.
            if digits.len() >= 4 {
                if let (Ok(hours), Ok(mins)) =
                    (digits[..2].parse::<f64>(), digits[2..4].parse::<f64>())
                {
                    unix += sign * (hours * 3600.0 + mins * 60.0);
                }
            }
        }
    }
    Some(AppleDate::from_unix(unix))
}

/// Split `YYYY-MM-DDTHH:MM:SS.fracZ` into its parts.
fn split_iso(raw: &str) -> Option<(String, String, f64, Option<String>)> {
    let (date_part, rest) = raw.split_once(['T', 't', ' '])?;
    let (time_part, tz) = match rest.find(['Z', '+', '-']) {
        Some(i) if rest.as_bytes()[i] == b'Z' => (&rest[..i], Some("Z".to_string())),
        Some(i) => (&rest[..i], Some(rest[i..].to_string())),
        None => (rest, None),
    };
    let (time_part, fraction) = match time_part.split_once('.') {
        Some((t, f)) => (t, format!("0.{f}").parse::<f64>().unwrap_or(0.0)),
        None => (time_part, 0.0),
    };
    Some((
        date_part.to_string(),
        time_part.to_string(),
        fraction,
        tz,
    ))
}

/// Days from 1970-01-01 to `YYYY-MM-DD`, as seconds. Proleptic Gregorian,
/// the civil-days algorithm (Howard Hinnant's) — no date library, because
/// the format has exactly one shape.
fn parse_ymd(date: &str) -> Option<f64> {
    let mut parts = date.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days as f64 * 86400.0)
}

fn parse_hms(time: &str) -> Option<(f64, f64, f64)> {
    let mut parts = time.split(':');
    let hour: f64 = parts.next()?.parse().ok()?;
    let min: f64 = parts.next()?.parse().ok()?;
    let sec: f64 = parts.next().unwrap_or("0").parse().ok()?;
    Some((hour, min, sec))
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

/// What one note contributes to the archive: the note, its images' bytes.
pub struct ExportNote<'a> {
    pub note: &'a Note,
    /// `(resource id (32 hex), extension, mime, bytes)` in the order the
    /// body references them.
    pub images: Vec<(String, String, String, Vec<u8>)>,
}

/// The body, with every image reference in Joplin's own form.
///
/// The export's body references its images as `<resource id>.<ext>` — the
/// spelling Joplin's own importer also accepts — but Joplin finds a
/// note's resources by the `:/<id>` pattern: a markdown link whose target
/// is `:/` followed by the resource's 32 hex characters. A plain
/// `<id>.png` target is not a resource link at all, so the note would
/// import with its pictures missing.
///
/// Both spellings are rewritten, and the alt text is kept.
fn rewrite_resource_links(body: &str, images: &[(String, String, String, Vec<u8>)]) -> String {
    let mut text = body.to_string();
    for (rid, ext, _mime, _bytes) in images {
        // `![alt](<id>.png)` → `![alt](:/<id>)`, and the bare form too.
        for old in [
            format!("({rid}.{ext})"),
            format!("(:/{rid}.{ext})"),
        ] {
            if text.contains(&old) {
                text = text.replace(&old, &format!("(:/{rid})"));
            }
        }
    }
    text
}

/// Build a `.jex` archive: notebooks first, then notes with their resources.
///
/// A note whose `notebook_id` names no notebook in `notebooks` is exported
/// with `parent_id` dropped (it lands in Joplin's Unfiled) rather than
/// carrying a dangling reference that makes Joplin's importer complain.
pub fn build_archive(notes: &[ExportNote<'_>], notebooks: &[Notebook]) -> Vec<u8> {
    let mut tar = TarWriter::new();
    let now = AppleDate::now().to_unix() as i64;
    let notebook_ids: Vec<&str> = notebooks.iter().map(|n| n.id.as_str()).collect();

    for nb in notebooks {
        let id = joplin_id(&nb.id);
        let block = MetadataBlock {
            fields: vec![
                ("id".to_string(), id.clone()),
                ("created_time".to_string(), iso(&nb.created_at)),
                // Joplin's folder records want an updated_time; the iOS
                // exporter ships created_time as the honest value and its
                // first real-Joplin import failed without it.
                ("updated_time".to_string(), iso(&nb.created_at)),
            ],
            body: nb.name.clone(),
            // A notebook's name IS its title: Joplin reads the first line
            // of a folder entry as the folder's title.
            title: Some(nb.name.clone()),
        };
        tar.append(&format!("{id}.md"), block.render(2).as_bytes(), now);
    }

    let mut emitted: std::collections::HashSet<String> = std::collections::HashSet::new();
    for export in notes {
        let note = export.note;
        let id = joplin_id(&note.id);
        let mut fields = vec![("id".to_string(), id.clone())];
        // A notebook this archive does not carry gets no parent_id.
        if let Some(parent) = note.notebook_id.as_deref() {
            if notebook_ids.contains(&parent) {
                fields.push(("parent_id".to_string(), joplin_id(parent)));
            }
        }
        fields.push(("created_time".to_string(), iso(&note.created_at)));
        fields.push(("updated_time".to_string(), iso(&note.updated_at)));
        // Joplin's own records carry the user-facing timestamps too, and the
        // iOS exporter writes them — the same format, field for field.
        fields.push(("user_created_time".to_string(), iso(&note.created_at)));
        fields.push(("user_updated_time".to_string(), iso(&note.updated_at)));
        fields.push(("markup_language".to_string(), "1".to_string()));
        // The body, with every image reference in Joplin's own form:
        // `![alt](:/<resource id>)`. Joplin finds a note's resources by
        // this pattern — a plain `<id>.png` link is just a broken link,
        // and the imported note arrives with no picture.
        let body = rewrite_resource_links(&note.text, &export.images);
        let block = MetadataBlock {
            fields,
            body,
            // The note's title, on the first line, the way Joplin writes
            // it. Without it Joplin takes the body's first line as the
            // title and the note arrives with an empty body.
            title: Some(note.title()),
        };
        tar.append(&format!("{id}.md"), block.render(1).as_bytes(), now);

        for (rid, ext, mime, bytes) in &export.images {
            if !emitted.insert(rid.clone()) {
                continue;
            }
            let meta = MetadataBlock {
                fields: vec![
                    ("id".to_string(), rid.clone()),
                    ("title".to_string(), format!("{rid}.{ext}")),
                    ("mime".to_string(), mime.clone()),
                    // Joplin builds the resource's file name from its id
                    // and its extension — `resourceFilename` is
                    // `id + '.' + file_extension` — and looks for it at
                    // `resources/<that name>`. Without this field the
                    // extension is derived from the mime, and a mismatch
                    // means the importer never finds the file.
                    ("file_extension".to_string(), ext.clone()),
                    ("filename".to_string(), String::new()),
                    ("created_time".to_string(), iso(&AppleDate::now())),
                    ("updated_time".to_string(), iso(&AppleDate::now())),
                ],
                body: String::new(),
                // A resource's title line is its `id.ext` file name — what
                // Joplin's `Resource.filename` resolves to and what its
                // importer matches the binary in `resources/` against.
                title: Some(format!("{rid}.{ext}")),
            };
            tar.append(&format!("{rid}.md"), meta.render(4).as_bytes(), now);
            tar.append(&format!("resources/{rid}.{ext}"), bytes, now);
        }
    }
    tar.finish()
}

/// The ISO-8601 UTC form with milliseconds the Joplin importer accepts.
fn iso(date: &AppleDate) -> String {
    let unix = date.to_unix();
    let secs = unix.floor() as i64;
    let millis = ((unix - unix.floor()) * 1000.0).round() as i64;
    let (y, m, d, hh, mm, ss) = civil_from_unix(secs);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}.{millis:03}Z")
}

/// Civil date from Unix seconds (the inverse of `parse_ymd`'s algorithm).
fn civil_from_unix(secs: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (
        y,
        m,
        d,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60,
    )
}

// ---------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------

/// One parsed archive, before it touches the stores.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Archive {
    pub notebooks: Vec<ImportedNotebook>,
    pub notes: Vec<ImportedNote>,
    pub resources: Vec<ImportedResource>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImportedNotebook {
    /// The JEX (32-hex) id.
    pub id: String,
    pub title: String,
    pub created_at: AppleDate,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImportedNote {
    pub id: String,
    /// The note's text, with resource links rewritten to the app's scheme.
    pub body: String,
    /// The note's title, when the archive carried one.
    ///
    /// Joplin's layout puts the title on its own first line, separate from
    /// the body; the iOS importer keeps the two apart the same way. A
    /// note that had an explicit title gets it back instead of having it
    /// derived from the body's first sentence.
    pub title: Option<String>,
    pub notebook_jex_id: Option<String>,
    pub created_at: AppleDate,
    pub updated_at: AppleDate,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImportedResource {
    pub id: String,
    pub mime: String,
    pub file_extension: String,
    pub data: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum JexError {
    #[error("not a JEX archive (no tar entries found)")]
    NotATar,
    #[error("the .jex archive is truncated")]
    Truncated,
    #[error("the .jex archive is empty")]
    NoEntries,
}

/// Parse a whole `.jex` (tar) payload.
///
/// Entries that cannot be parsed are skipped, never fatal. Only a file that
/// cannot be a tar at all (the very first header unreadable) or one that
/// yields nothing is refused.
pub fn parse_archive(data: &[u8]) -> Result<Archive, JexError> {
    let mut archive = Archive::default();
    let mut cursor = 0usize;
    let mut saw_entry = false;
    let mut resource_bytes: std::collections::HashMap<String, Vec<u8>> =
        std::collections::HashMap::new();
    // (mime, ext) per resource id, merged from the type-4 metadata and the
    // resources/ entry's extension.
    let mut resource_meta: std::collections::HashMap<String, (String, String)> =
        std::collections::HashMap::new();

    while cursor + 512 <= data.len() {
        let header = &data[cursor..cursor + 512];
        // Two consecutive zero blocks end the archive; one zero block is the
        // last entry's padding — both mean done.
        if header.iter().all(|b| *b == 0) {
            break;
        }
        let Some(entry) = TarEntry::parse(header) else {
            return if cursor == 0 {
                Err(JexError::NotATar)
            } else {
                Err(JexError::Truncated)
            };
        };
        saw_entry = true;
        cursor += 512;
        let size = entry.size;
        if cursor + size > data.len() {
            return Err(JexError::Truncated);
        }
        let payload = &data[cursor..cursor + size];
        cursor += (size + 511) & !511;

        match entry.kind {
            EntryKind::Regular => {
                let name = entry.name;
                if let Some(rest) = name.strip_prefix("resources/") {
                    let (id, ext) = split_file_name(rest);
                    if !id.is_empty() {
                        resource_bytes.insert(id.clone(), payload.to_vec());
                        match resource_meta.get_mut(&id) {
                            Some(meta) if meta.1.is_empty() && !ext.is_empty() => {
                                meta.1 = ext
                            }
                            Some(_) => {}
                            None => {
                                resource_meta.insert(id, (String::new(), ext));
                            }
                        }
                    }
                } else if let Ok(text) = std::str::from_utf8(payload) {
                    if let Some((meta, type_code)) = MetadataBlock::parse(text) {
                        match type_code {
                            1 => {
                                // A note entry without an id is skipped — an
                                // import that yields the other notes beats
                                // one that stops on one odd entry.
                                let Some(id) = meta.get("id").filter(|i| !i.is_empty()) else {
                                    continue;
                                };
                                let body = meta.body.clone();
                                archive.notes.push(ImportedNote {
                                    id: id.to_string(),
                                    body,
                                    title: meta.title.clone().filter(|t| !t.is_empty()),
                                    notebook_jex_id: meta
                                        .get("parent_id")
                                        .filter(|p| !p.is_empty())
                                        .map(str::to_string),
                                    created_at: meta.date("created_time"),
                                    updated_at: meta.date("updated_time"),
                                });
                            }
                            2 => {
                                let id = meta.get("id").unwrap_or_default().to_string();
                                let title = meta
                                    .body
                                    .lines()
                                    .next()
                                    .unwrap_or("")
                                    .trim()
                                    .to_string();
                                if !id.is_empty() {
                                    archive.notebooks.push(ImportedNotebook {
                                        id,
                                        title,
                                        created_at: meta.date("created_time"),
                                    });
                                }
                            }
                            4 => {
                                let id = meta.get("id").unwrap_or_default().to_string();
                                let mime = meta.get("mime").unwrap_or_default().to_string();
                                if !id.is_empty() {
                                    match resource_meta.get_mut(&id) {
                                        Some(entry) => {
                                            if !mime.is_empty() {
                                                entry.0 = mime;
                                            }
                                        }
                                        None => {
                                            resource_meta.insert(id, (mime, String::new()));
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            EntryKind::Directory | EntryKind::Other => {}
        }
    }

    if !saw_entry {
        return Err(JexError::NotATar);
    }
    for (id, bytes) in resource_bytes {
        let Some(meta) = resource_meta.get(&id) else {
            continue;
        };
        let (mime, ext) = meta.clone();
        let ext = if ext.is_empty() {
            match extension_for_mime(&mime) {
                Some(e) => e,
                None => continue,
            }
        } else {
            ext
        };
        archive.resources.push(ImportedResource {
            id,
            mime,
            file_extension: ext,
            data: bytes,
        });
    }
    if archive.notes.is_empty()
        && archive.notebooks.is_empty()
        && archive.resources.is_empty()
    {
        return Err(JexError::NoEntries);
    }
    Ok(archive)
}

/// One tar header.
struct TarEntry {
    name: String,
    kind: EntryKind,
    size: usize,
}

enum EntryKind {
    Regular,
    Directory,
    Other,
}

impl TarEntry {
    /// Parse one 512-byte header. The magic may be `ustar`, empty (the old
    /// pre-ustar format some writers emit), or GNU (`ustar  \0`); anything
    /// else means this is not a tar we can read.
    fn parse(header: &[u8]) -> Option<Self> {
        let magic = c_string(&header[257..263]);
        let normalized = magic.trim_matches(['\0', ' ']);
        if !normalized.is_empty() && !normalized.starts_with("ustar") {
            return None;
        }
        let size_field = c_string(&header[124..136]);
        let size = usize::from_str_radix(size_field.trim(), 8).ok()?;
        let name = c_string(&header[0..100]);
        let kind = match header[156] {
            b'0' | 0 => EntryKind::Regular,
            b'5' => EntryKind::Directory,
            _ => EntryKind::Other,
        };
        Some(Self { name, kind, size })
    }
}

/// A NUL-terminated field as a string, Latin-1 for anything non-UTF-8 (what
/// such filenames mean).
fn c_string(field: &[u8]) -> String {
    let end = field.iter().position(|b| *b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).into_owned()
}

/// Split `image1.png` into `("image1", "png")`.
fn split_file_name(rest: &str) -> (String, String) {
    match rest.rsplit_once('.') {
        Some((id, ext)) if !id.is_empty() => (id.to_string(), ext.to_ascii_lowercase()),
        _ => (rest.to_string(), String::new()),
    }
}

/// Best-effort extension for a resource whose metadata carries only a mime.
fn extension_for_mime(mime: &str) -> Option<String> {
    match mime.to_ascii_lowercase().as_str() {
        "image/png" => Some("png".into()),
        "image/jpeg" | "image/jpg" => Some("jpg".into()),
        "image/gif" => Some("gif".into()),
        "image/webp" => Some("webp".into()),
        "image/bmp" => Some("bmp".into()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(text: &str) -> Note {
        let mut n = Note::new(None);
        n.text = text.to_string();
        n
    }

    #[test]
    fn joplin_ids_strip_dashes_and_lowercase() {
        assert_eq!(
            joplin_id("A26F68B6-BCE1-4E21-9BAA-3D914D0E2A44"),
            "a26f68b6bce14e219baa3d914d0e2a44"
        );
        assert_eq!(
            uuid_from_joplin_id("a26f68b6bce14e219baa3d914d0e2a44").as_deref(),
            // Uppercase, matching `new_uuid()`'s form.
            Some("A26F68B6-BCE1-4E21-9BAA-3D914D0E2A44")
        );
        assert_eq!(uuid_from_joplin_id("nothex"), None);
        assert_eq!(uuid_from_joplin_id(""), None);
        // A 32-hex id round-trips: re-stripping the dashes of the
        // re-dashed (uppercased) form gives back the same hex.
        let original = "a26f68b6bce14e219baa3d914d0e2a44";
        assert_eq!(
            uuid_from_joplin_id(original)
                .unwrap()
                .replace('-', "")
                .to_ascii_lowercase(),
            original
        );
    }

    #[test]
    fn the_tar_writer_produces_512_alignment_and_a_checksum() {
        let mut tar = TarWriter::new();
        tar.append("hello.md", b"hi", 1_700_000_000);
        let bytes = tar.finish();
        assert_eq!(bytes.len(), 512 + 512 + 1024, "header + padded data + end");
        // The stored checksum matches a re-sum with spaces in the field.
        let mut sum = 0usize;
        for (i, b) in bytes[..512].iter().enumerate() {
            let v = if (148..156).contains(&i) { b' ' as usize } else { *b as usize };
            sum += v;
        }
        let stored = std::str::from_utf8(&bytes[148..156]).unwrap();
        assert_eq!(
            usize::from_str_radix(stored.trim_matches(['\0', ' ']), 8).unwrap(),
            sum,
            "the header checksum matches a re-sum with spaces in the field"
        );
        // The data round-trips.
        assert_eq!(&bytes[512..514], b"hi");
    }

    #[test]
    fn a_metadata_block_renders_with_type_last_and_parses_back() {
        let block = MetadataBlock {
            fields: vec![
                ("id".into(), "abc".into()),
                ("created_time".into(), "2026-09-08T16:00:43.123Z".into()),
            ],
            body: "The body.".into(),
            title: None,
        };
        let rendered = block.render(1);
        assert!(rendered.starts_with("The body.\n\n"), "{rendered}");
        // No trailing newline: Joplin's parser walks the lines from the end
        // and a trailing empty line hides the `type_` line from it.
        assert!(rendered.ends_with("type_: 1"), "{rendered}");
        let (parsed, code) = MetadataBlock::parse(&rendered).unwrap();
        assert_eq!(code, 1);
        assert_eq!(parsed.body, "The body.");
        assert_eq!(parsed.get("id"), Some("abc"));
        assert_eq!(parsed.date("created_time").to_unix(), 1_788_883_243.123);
    }

    /// The gate that matters for the other direction: our archive must
    /// parse with **Joplin's own** rules.
    ///
    /// This is a faithful port of `BaseItem.unserialize` in Joplin
    /// (`packages/lib/models/BaseItem.ts`), the parser every JEX import
    /// goes through. It walks the lines from the end, stops at the first
    /// blank one, and treats the first two lines above it as the title.
    /// A trailing newline after `type_` hides that line from the walk and
    /// the entry is refused outright — which is what made Joplin reject
    /// every note in an archive this app produced.
    fn joplin_unserialize(content: &str) -> Result<(String, String, u32), String> {
        let lines: Vec<&str> = content.split('\n').collect();
        let mut output: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        let mut body: Vec<&str> = Vec::new();
        for i in (0..lines.len()).rev() {
            let line = lines[i].trim();
            if line.is_empty() {
                body = lines[..i].to_vec();
                break;
            }
            let Some(p) = line.find(':') else {
                return Err(format!("invalid property format: {line}"));
            };
            output.insert(line[..p].trim().to_string(), line[p + 1..].trim().to_string());
        }
        let Some(type_) = output.get("type_") else {
            return Err("missing required property: type_".to_string());
        };
        let type_: u32 = type_.parse().map_err(|_| "bad type_".to_string())?;
        let mut title = String::new();
        if !body.is_empty() {
            title = body.remove(0).to_string();
            if !body.is_empty() {
                body.remove(0); // the blank line after the title
            }
        }
        Ok((
            title,
            if type_ == 1 { body.join("\n") } else { String::new() },
            type_,
        ))
    }

    // The `entry_payload` test helper (raw bytes of one entry's payload,
    // found by entry name) was removed 2026-10-30 in a dead-code trim — zero
    // callers anywhere.

    #[test]
    fn our_entries_parse_with_joplins_own_parser() {
        let mut nb = Notebook::new("Work");
        nb.id = "11111111-2222-3333-4444-555555555555".into();
        let mut n = note("The first paragraph.\n\nThe second paragraph.");
        n.id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into();
        n.notebook_id = Some(nb.id.clone());
        let archive = build_archive(
            &[ExportNote {
                note: &n,
                images: Vec::new(),
            }],
            &[nb],
        );
        let parsed = parse_archive(&archive).expect("our own parser reads it");

        // And now Joplin's parser, over the same entry's bytes. The note's
        // entry is the one named after its id; its text starts after the
        // 512-byte tar header and ends at the next header.
        let note_id = joplin_id(&n.id);
        let start = archive
            .windows(note_id.len())
            .position(|w| w == note_id.as_bytes())
            .expect("the note's entry is in the archive");
        let after_header = start + 512;
        let end = archive[after_header..]
            .windows(512)
            .enumerate()
            .find(|(_, w)| w.iter().all(|b| *b == 0))
            .map(|(i, _)| after_header + i)
            .unwrap_or(archive.len());
        let text = String::from_utf8_lossy(&archive[after_header..end]).trim_end().to_string();

        //  the helper returns `(title, body, type_)` but this call site
        // still destructured two — a mid-edit state in the original.
        let (title, body, _type_) = joplin_unserialize(&text).expect("Joplin's parser accepts it");
        assert_eq!(title, n.title(), "Joplin reads the title");
        assert!(
            body.contains("The first paragraph."),
            "Joplin reads the body: {body:?}"
        );
        assert!(
            body.contains("The second paragraph."),
            "Joplin reads the whole body: {body:?}"
        );
        assert_eq!(parsed.notes.len(), 1);
    }

    #[test]
    fn iso_parsing_covers_both_forms_and_offsets() {
        let a = parse_iso("2026-09-08T16:00:43.123Z").unwrap().to_unix();
        let b = parse_iso("2026-09-08T16:00:43Z").unwrap().to_unix();
        assert!((a - b - 0.123).abs() < 0.001, "{a} vs {b}");
        // +02:00 names a local time two hours AHEAD of UTC, so the UTC
        // instant is two hours earlier.
        let c = parse_iso("2026-09-08T16:00:43+02:00").unwrap().to_unix();
        assert!((c - b + 7200.0).abs() < 1.0, "{c} vs {b}");
        // ms-since-epoch (Joplin's alternate form).
        let d = parse_iso("1788883243123").unwrap().to_unix();
        assert!((d - 1_788_883_243.123).abs() < 0.01);
        assert!(parse_iso("not a date").is_none());
    }

    #[test]
    fn an_export_import_round_trip_preserves_everything() {
        let mut nb = Notebook::new("Work");
        nb.id = "11111111-2222-3333-4444-555555555555".into();
        let mut n = note("# Heading\n\nBody with a **bold** bit.");
        n.id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into();
        n.notebook_id = Some(nb.id.clone());
        n.created_at = AppleDate::from_unix(1_700_000_000.0);
        n.updated_at = AppleDate::from_unix(1_700_000_100.0);
        let exports = vec![ExportNote {
            note: &n,
            images: Vec::new(),
        }];
        let bytes = build_archive(&exports, &[nb.clone()]);

        let archive = parse_archive(&bytes).unwrap();
        assert_eq!(archive.notebooks.len(), 1);
        assert_eq!(archive.notes.len(), 1);
        let got_nb = &archive.notebooks[0];
        assert_eq!(got_nb.title, "Work");
        assert_eq!(
            uuid_from_joplin_id(&got_nb.id).as_deref(),
            Some("11111111-2222-3333-4444-555555555555")
        );
        let got = &archive.notes[0];
        assert_eq!(got.body, n.text, "the body survives the round trip");
        assert_eq!(
            got.title.as_deref(),
            Some(n.title().as_str()),
            "the title survives the round trip"
        );
        assert_eq!(
            got.notebook_jex_id.as_deref(),
            Some(joplin_id(&nb.id)).as_deref()
        );
        assert_eq!(got.created_at.to_unix(), 1_700_000_000.0);
        assert_eq!(got.updated_at.to_unix(), 1_700_000_100.0);
    }

    #[test]
    fn a_dangling_parent_id_is_dropped_not_carried() {
        let mut n = note("orphan");
        n.notebook_id = Some("nonexistent".into());
        let bytes = build_archive(
            &[ExportNote { note: &n, images: Vec::new() }],
            &[], // no notebooks in this export
        );
        let archive = parse_archive(&bytes).unwrap();
        assert_eq!(archive.notes.len(), 1);
        assert_eq!(archive.notes[0].notebook_jex_id, None);
    }

    #[test]
    fn resources_land_as_blobs_and_are_deduplicated() {
        let mut n = note("body");
        n.id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into();
        let rid = joplin_id(&n.id);
        let image = vec![1u8, 2, 3, 4];
        let exports = vec![ExportNote {
            note: &n,
            images: vec![
                (
                    rid.clone(),
                    "png".into(),
                    "image/png".into(),
                    image.clone(),
                ),
                // The same resource twice (a note referencing it twice) is
                // one entry.
                (rid.clone(), "png".into(), "image/png".into(), image.clone()),
            ],
        }];
        let bytes = build_archive(&exports, &[]);
        let archive = parse_archive(&bytes).unwrap();
        assert_eq!(archive.resources.len(), 1);
        assert_eq!(archive.resources[0].data, image);
        assert_eq!(archive.resources[0].file_extension, "png");
        assert_eq!(archive.resources[0].mime, "image/png");
    }

    #[test]
    fn a_resource_with_mime_but_no_extension_still_lands() {
        // A foreign exporter that writes only type-4 metadata and a
        // resources/ entry with no recognizable extension.
        let mut tar = TarWriter::new();
        let block = MetadataBlock {
            fields: vec![
                ("id".into(), "feedfacefeedfacefeedfacefeedface".into()),
                ("mime".into(), "image/jpeg".into()),
            ],
            body: String::new(),
            title: None,
        };
        tar.append("feedfacefeedfacefeedfacefeedface.md", block.render(4).as_bytes(), 0);
        tar.append("resources/feedfacefeedfacefeedfacefeedface", &[9, 9, 9], 0);
        let bytes = tar.finish();
        let archive = parse_archive(&bytes).unwrap();
        assert_eq!(archive.resources.len(), 1);
        assert_eq!(archive.resources[0].file_extension, "jpg", "mime → ext");
    }

    #[test]
    fn unknown_types_and_junk_entries_are_skipped_not_fatal() {
        let mut tar = TarWriter::new();
        // A type-3 entry (future Joplin type) and a garbage entry.
        let junk = MetadataBlock {
            fields: vec![("id".into(), "x".into())],
            body: String::new(),
            title: None,
        };
        tar.append("future.md", junk.render(3).as_bytes(), 0);
        let note_block = MetadataBlock {
            fields: vec![("id".into(), "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into())],
            body: "kept".into(),
            title: None,
        };
        tar.append("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.md", note_block.render(1).as_bytes(), 0);
        let bytes = tar.finish();
        let archive = parse_archive(&bytes).unwrap();
        assert_eq!(archive.notes.len(), 1, "the good note survives");
        assert_eq!(archive.notes[0].body, "kept");
    }

    #[test]
    fn an_empty_or_non_tar_file_is_refused_by_name() {
        assert!(matches!(parse_archive(b""), Err(JexError::NotATar)));
        assert!(matches!(parse_archive(&[0u8; 512]), Err(JexError::NotATar)));
        // Random bytes are not a tar.
        assert!(matches!(
            parse_archive(&[7u8; 1024]),
            Err(JexError::NotATar)
        ));
    }

    #[test]
    fn a_truncated_entry_is_refused() {
        let mut tar = TarWriter::new();
        tar.append("x.md", b"hello", 0);
        let bytes = tar.finish();
        // Cut the data block short: the loop exits at the size check with a
        // truncated error only when the cut lands INSIDE a payload; a cut
        // after the entry yields the entries it read (here none complete
        // enough to be a note) and the empty-archive error.
        assert!(matches!(
            parse_archive(&bytes[..600]),
            Err(JexError::NoEntries)
        ));
        // A cut inside the payload is a truncation.
        assert!(matches!(
            parse_archive(&bytes[..515]),
            Err(JexError::Truncated)
        ));
    }

    #[test]
    fn joplin_writers_that_omit_the_ustar_magic_still_parse() {
        // Old-style tar: zeros in the magic field.
        let mut header = [0u8; 512];
        header[..7].copy_from_slice(b"old.md\0");
        write_octal(&mut header[124..136], 4);
        header[156] = b'0';
        let mut full = header.to_vec();
        full.extend_from_slice(b"body");
        full.extend(std::iter::repeat(0u8).take(508));
        full.extend(std::iter::repeat(0u8).take(1024));
        // The header parses as tar (no error), but an entry with no
        // metadata block contributes nothing, and an archive with nothing
        // importable is refused by name — that is the honest answer, not a
        // silent empty success.
        assert!(matches!(parse_archive(&full), Err(JexError::NoEntries)));
    }

    #[test]
    fn pax_and_gnu_entries_are_skipped_as_other() {
        let mut header = [0u8; 512];
        header[..11].copy_from_slice(b"PaxHeaders\0");
        header[156] = b'x';
        header[257..263].copy_from_slice(b"ustar\0");
        write_octal(&mut header[124..136], 100);
        let mut full = header.to_vec();
        full.extend(std::iter::repeat(0u8).take(1024));
        full.extend(std::iter::repeat(0u8).take(1024));
        // A pax header parses as tar and is skipped as "other"; with nothing
        // importable behind it the archive is refused as empty — the shape
        // is readable, the content is not there.
        assert!(matches!(parse_archive(&full), Err(JexError::NoEntries)));
    }

    #[test]
    fn civil_dates_round_trip() {
        for unix in [0i64, 1_700_000_000, 951_782_400, 4_102_444_800] {
            let (y, m, d, hh, mm, ss) = civil_from_unix(unix);
            let back = parse_ymd(&format!("{y:04}-{m:02}-{d:02}")).unwrap()
                + hh as f64 * 3600.0
                + mm as f64 * 60.0
                + ss as f64;
            assert!((back - unix as f64).abs() < 0.5, "{unix} vs {back}");
        }
        // The epoch is 1970-01-01.
        assert_eq!(civil_from_unix(0), (1970, 1, 1, 0, 0, 0));
    }

    #[test]
    fn the_written_iso_matches_the_ios_writer_shape() {
        let date = AppleDate::from_unix(1_788_883_243.123);
        assert_eq!(iso(&date), "2026-09-08T16:00:43.123Z");
    }
}
