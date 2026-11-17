//! A read-only ZIP reader — enough for EPUB containers.
//!
//! The traps this module exists to hold, all inherited from the iOS port's
//! hand-rolled reader (the Android port shipped a full `ZipReader` rewrite
//! that still got two of them wrong):
//!
//! * **Central-directory parsing is by the book.** The EOCD is found by
//!   scanning backwards for its signature with room for a comment; the
//!   directory offset is then *relative to the start of the archive* — an
//!   archive with a prepended blob (self-extracting stub, or a
//!   `<directoryOffset>` mismatch) is handled by anchoring on the EOCD's own
//!   position, not the declared offset, when the two disagree.
//! * **Local headers lie.** A local file header's compressed size can be
//!   0 with a data descriptor following the payload; the *central*
//!   directory's sizes are the authoritative ones, so entries are read
//!   through their central-directory record and the payload is cut by its
//!   length rather than trusting the local header.
//! * **CRC-32 is verified.** A truncated or corrupt DEFLATE payload of a
//!   plausible length passes every size check; the CRC catches it.
//! * **UTF-8 names only.** The general-purpose flag's bit 11 marks UTF-8
//!   names; anything else is CP437-decoded the way the spec says, and
//!   directory entries end in `/`.
//!
//! Only `stored` (0) and `deflate` (8) — everything an EPUB ever uses.
//! Encrypted entries and ZIP64 fail with explicit errors rather than
//! producing corrupt data.

use std::collections::HashMap;

/// One entry in the archive's central directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Entry path as stored, with `/` separators.
    pub name: String,
    /// 0 = stored, 8 = deflate.
    pub method: u16,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
    /// Offset of the entry's local file header.
    pub local_offset: u64,
    /// CRC-32 of the uncompressed payload.
    pub crc32: u32,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.name.ends_with('/')
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ZipError {
    #[error("not a zip file: no end-of-central-directory record")]
    NotAZip,
    #[error("zip64 is not supported")]
    UnsupportedZip64,
    #[error("{name} is encrypted")]
    Encrypted { name: String },
    #[error("{name} uses unsupported compression method {method}")]
    UnsupportedMethod { name: String, method: u16 },
    #[error("entry {name:?} not found")]
    NotFound { name: String },
    #[error("corrupt archive: {0}")]
    Corrupt(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// A whole archive in memory. EPUBs are tens of megabytes; the reader never
/// copies the payload — `read` slices it out of the archive.
///
/// The bytes sit behind an `Arc` so a cached archive can be handed out
/// cheaply: `clone_shallow` shares one copy of the file between every caller
/// (the reader's search, the shelf's progress, the chapter pane) instead of
/// cloning hundreds of megabytes per call.
#[derive(Clone)]
pub struct Archive {
    data: std::sync::Arc<Vec<u8>>,
    entries: std::sync::Arc<HashMap<String, Entry>>,
}

/// Read-only access to a ZIP archive.
impl Archive {
    /// Parse an archive from whole bytes.
    pub fn parse(data: Vec<u8>) -> Result<Self, ZipError> {
        let eocd = find_eocd(&data)?;
        if eocd.zip64 {
            return Err(ZipError::UnsupportedZip64);
        }
        // The declared directory offset can disagree with where the directory
        // actually sits (a prepended stub). The EOCD's own position is the
        // truth: the directory ends immediately before it.
        let dir_end = eocd.record_position as usize;
        let dir_start = dir_end
            .checked_sub(eocd.directory_size)
            .ok_or_else(|| ZipError::Corrupt("central directory is larger than the file".into()))?;
        let entries = parse_central_directory(&data, dir_start, eocd.total_entries)?;
        let mut map = HashMap::with_capacity(entries.len());
        for entry in entries {
            map.insert(entry.name.clone(), entry);
        }
        Ok(Self {
            data: std::sync::Arc::new(data),
            entries: std::sync::Arc::new(map),
        })
    }

    /// A handle onto the same archive: shares the file bytes and the entry
    /// table, costs two reference counts.
    pub fn clone_shallow(&self) -> Self {
        Self {
            data: self.data.clone(),
            entries: self.entries.clone(),
        }
    }

    /// Every entry, sorted by name.
    pub fn entries(&self) -> Vec<&Entry> {
        let mut all: Vec<&Entry> = self.entries.values().collect();
        all.sort_by(|a, b| a.name.cmp(&b.name));
        all
    }

    /// Does this archive hold an entry by this exact name?
    pub fn contains(&self, name: &str) -> bool {
        self.entries.contains_key(name)
    }

    /// Read one entry's uncompressed bytes. The name must match exactly —
    /// EPUB paths are case-sensitive, and the reader never guesses.
    ///
    /// A declared uncompressed size past [`MAX_MEMBER_BYTES`] is refused
    /// before a byte is decompressed: the size field is attacker-chosen,
    /// and honouring it is how a 4 MB "book" asks for a 4 GiB allocation.
    pub fn read(&self, name: &str) -> Result<Vec<u8>, ZipError> {
        let entry = self
            .entries
            .get(name)
            .ok_or_else(|| ZipError::NotFound { name: name.to_string() })?;
        if entry.is_dir() {
            return Err(ZipError::NotFound { name: name.to_string() });
        }
        if entry.uncompressed_size > MAX_MEMBER_BYTES as u64 {
            return Err(ZipError::Corrupt(format!(
                "{name}: declares {} bytes uncompressed; over the {} MiB member cap",
                entry.uncompressed_size,
                MAX_MEMBER_BYTES / (1024 * 1024)
            )));
        }
        if entry.method == 8 {
            // Deflate: the payload starts after the local header's own
            // name/extra fields.
            let header_end = local_header_end(&self.data, entry)?;
            let start = header_end as usize;
            let end = start
                .checked_add(entry.compressed_size as usize)
                .filter(|e| *e <= self.data.len())
                .ok_or_else(|| ZipError::Corrupt(format!("{name}: payload out of bounds")))?;
            let out = inflate(&self.data[start..end], entry.uncompressed_size as usize)?;
            if crc32(&out) != entry.crc32 {
                return Err(ZipError::Corrupt(format!("{name}: CRC-32 mismatch")));
            }
            Ok(out)
        } else if entry.method == 0 {
            if entry.compressed_size > MAX_MEMBER_BYTES as u64 {
                return Err(ZipError::Corrupt(format!(
                    "{name}: a stored member over the {} MiB cap",
                    MAX_MEMBER_BYTES / (1024 * 1024)
                )));
            }
            let start = local_header_end(&self.data, entry)? as usize;
            let end = start
                .checked_add(entry.compressed_size as usize)
                .filter(|e| *e <= self.data.len())
                .ok_or_else(|| ZipError::Corrupt(format!("{name}: payload out of bounds")))?;
            let out = self.data[start..end].to_vec();
            if crc32(&out) != entry.crc32 {
                return Err(ZipError::Corrupt(format!("{name}: CRC-32 mismatch")));
            }
            Ok(out)
        } else {
            Err(ZipError::UnsupportedMethod {
                name: name.to_string(),
                method: entry.method,
            })
        }
    }

    /// The archive's own bytes — the library copy of a book is written from
    /// what was already parsed, not read off the disk a second time.
    pub fn raw(&self) -> &[u8] {
        &self.data
    }

    /// Read an entry as UTF-8, replacing invalid sequences. EPUB XML is
    /// UTF-8; a stray byte in a malformed book must not fail the shelf.
    pub fn read_string(&self, name: &str) -> Result<String, ZipError> {
        let bytes = self.read(name)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

/// The end-of-central-directory record, plus where the record itself sits.
struct Eocd {
    total_entries: usize,
    directory_size: usize,
    zip64: bool,
    /// Byte offset of the EOCD signature in the file — the anchor.
    record_position: u64,
}

/// Scan backwards for the EOCD signature, allowing a 64 KB comment.
fn find_eocd(data: &[u8]) -> Result<Eocd, ZipError> {
    const SIG: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];
    let min = data.len().saturating_sub(22 + 0xFFFF);
    let mut pos = data.len().saturating_sub(22);
    loop {
        if pos < min {
            return Err(ZipError::NotAZip);
        }
        if pos + 22 <= data.len() && data[pos..pos + 4] == SIG {
            let comment_len = u16::from_le_bytes([data[pos + 20], data[pos + 21]]) as usize;
            if pos + 22 + comment_len == data.len() {
                return Ok(Eocd {
                    total_entries: u16::from_le_bytes([data[pos + 10], data[pos + 11]]) as usize,
                    directory_size: u32::from_le_bytes([
                        data[pos + 12],
                        data[pos + 13],
                        data[pos + 14],
                        data[pos + 15],
                    ]) as usize,
                    // The sentinel values mean "look at zip64", which this
                    // reader does not support.
                    zip64: u16::from_le_bytes([data[pos + 10], data[pos + 11]]) == 0xFFFF
                        || u32::from_le_bytes([
                            data[pos + 12],
                            data[pos + 13],
                            data[pos + 14],
                            data[pos + 15],
                        ]) == 0xFFFF_FFFF
                        || u32::from_le_bytes([
                            data[pos + 16],
                            data[pos + 17],
                            data[pos + 18],
                            data[pos + 19],
                        ]) == 0xFFFF_FFFF,
                    record_position: pos as u64,
                });
            }
        }
        if pos == 0 {
            return Err(ZipError::NotAZip);
        }
        pos -= 1;
    }
}

/// Parse the central directory, counting entries rather than trusting a
/// stale total when the two disagree.
fn parse_central_directory(
    data: &[u8],
    start: usize,
    declared: usize,
) -> Result<Vec<Entry>, ZipError> {
    let mut entries = Vec::new();
    let mut pos = start;
    const SIG: [u8; 4] = [0x50, 0x4b, 0x01, 0x02];
    while pos + 46 <= data.len() {
        if data[pos..pos + 4] != SIG {
            // Not a central-directory header here: either the end of the
            // directory or a corrupt offset.
            break;
        }
        let flags = u16::from_le_bytes([data[pos + 8], data[pos + 9]]);
        if flags & 0x0001 != 0 {
            let name_len = u16::from_le_bytes([data[pos + 28], data[pos + 29]]) as usize;
            let name = read_name(data, pos + 46, name_len, flags);
            return Err(ZipError::Encrypted { name });
        }
        let method = u16::from_le_bytes([data[pos + 10], data[pos + 11]]);
        let crc32 = u32::from_le_bytes([
            data[pos + 16],
            data[pos + 17],
            data[pos + 18],
            data[pos + 19],
        ]);
        let compressed_size = u32::from_le_bytes([
            data[pos + 20],
            data[pos + 21],
            data[pos + 22],
            data[pos + 23],
        ]) as u64;
        let uncompressed_size = u32::from_le_bytes([
            data[pos + 24],
            data[pos + 25],
            data[pos + 26],
            data[pos + 27],
        ]) as u64;
        let name_len = u16::from_le_bytes([data[pos + 28], data[pos + 29]]) as usize;
        let extra_len = u16::from_le_bytes([data[pos + 30], data[pos + 31]]) as usize;
        let comment_len = u16::from_le_bytes([data[pos + 32], data[pos + 33]]) as usize;
        let local_offset = u32::from_le_bytes([
            data[pos + 42],
            data[pos + 43],
            data[pos + 44],
            data[pos + 45],
        ]) as u64;
        let name = read_name(data, pos + 46, name_len, flags);
        entries.push(Entry {
            name,
            method,
            compressed_size,
            uncompressed_size,
            local_offset,
            crc32,
        });
        pos += 46 + name_len + extra_len + comment_len;
    }
    if entries.len() != declared {
        // A stale declared count is a broken archive; refuse rather than
        // silently truncating the entry list (the Android port's bug).
        return Err(ZipError::Corrupt(format!(
            "central directory holds {} entries, declares {declared}", entries.len()
        )));
    }
    Ok(entries)
}

/// The name at `offset`, decoded as UTF-8 (flag bit 11) or CP437.
fn read_name(data: &[u8], offset: usize, len: usize, flags: u16) -> String {
    let bytes = &data[offset..(offset + len).min(data.len())];
    if flags & 0x0800 != 0 {
        String::from_utf8_lossy(bytes).into_owned()
    } else {
        bytes.iter().map(|&b| cp437(b)).collect()
    }
}

/// The CP437 mapping for one byte — the ZIP spec's non-UTF-8 fallback.
fn cp437(b: u8) -> char {
    const TABLE: [char; 128] = [
        'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å',
        'É', 'æ', 'Æ', 'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', '¢', '£', '¥', '₧', 'ƒ',
        'á', 'í', 'ó', 'ú', 'ñ', 'Ñ', 'ª', 'º', '¿', '⌐', '¬', '½', '¼', '¡', '«', '»',
        '░', '▒', '▓', '│', '┤', '╡', '╢', '╖', '╕', '╣', '║', '╗', '╝', '╜', '╛', '┐',
        '└', '┴', '┬', '├', '─', '┼', '╞', '╟', '╚', '╔', '╩', '╦', '╠', '═', '╬', '╧',
        '╨', '╤', '╥', '╙', '╘', '╒', '╓', '╫', '╪', '┘', '┌', '█', '▄', '▌', '▐', '▀',
        'α', 'ß', 'Γ', 'π', 'Σ', 'σ', 'µ', 'τ', 'Φ', 'Θ', 'Ω', 'δ', '∞', 'φ', 'ε', '∩',
        '≡', '±', '≥', '≤', '⌠', '⌡', '÷', '≈', '°', '∙', '·', '√', 'ⁿ', '²', '■', ' ',
    ];
    if b < 0x80 {
        b as char
    } else {
        TABLE[(b - 0x80) as usize]
    }
}

/// Where a local header ends — its name and extra fields are read, not
/// assumed, because the local header's own sizes are the fields the spec
/// allows to be zeroed.
fn local_header_end(data: &[u8], entry: &Entry) -> Result<u64, ZipError> {
    let start = entry.local_offset as usize;
    if start + 30 > data.len() || &data[start..start + 4] != b"PK\x03\x04" {
        return Err(ZipError::Corrupt(format!(
            "{}: no local header at {}",
            entry.name, entry.local_offset
        )));
    }
    let name_len = u16::from_le_bytes([data[start + 26], data[start + 27]]) as usize;
    let extra_len = u16::from_le_bytes([data[start + 28], data[start + 29]]) as usize;
    Ok((start + 30 + name_len + extra_len) as u64)
}

/// Raw DEFLATE (RFC 1951) decompression with a known output size, through
/// `flate2`'s zlib backend.
///
/// The output is bounded twice: the caller has already refused declared
/// sizes over [`MAX_MEMBER_BYTES`], and the buffer is NOT preallocated at
/// the declared size — a small lie about `expected` used to become a
/// `with_capacity` of gigabytes before a single byte decompressed. The
/// buffer grows with the bytes that actually come out.
fn inflate(data: &[u8], expected: usize) -> Result<Vec<u8>, ZipError> {
    use flate2::read::DeflateDecoder;
    use std::io::Read;
    // Read one byte past the declared size: a payload that ends early then
    // fails the CRC (the check the caller runs) instead of read_to_end
    // quietly returning a short buffer that no size check catches.
    let mut decoder = DeflateDecoder::new(data).take(expected as u64 + 1);
    let mut out = Vec::with_capacity(expected.min(4 * 1024 * 1024) + 1);
    decoder
        .read_to_end(&mut out)
        .map_err(|e| ZipError::Corrupt(format!("deflate failed: {e}")))?;
    Ok(out)
}

/// The largest member this reader will decompress. The heaviest honest
/// member of any EPUB, docx or JEX is an image measured in single-digit
/// megabytes; 256 MiB is generous past reason and small enough that a
/// hostile archive cannot use one "member" to own the machine's memory.
const MAX_MEMBER_BYTES: usize = 256 * 1024 * 1024;

/// CRC-32 (IEEE), the checksum ZIP stores. A table-driven implementation;
/// entries are small and this runs once per read.
pub fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, slot) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *slot = c;
        }
        t
    });
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc = table[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

/// A stored-entry ZIP holding several files — the fixture the book
/// parsers' tests share (an `.htmlz` is exactly this: `index.html`, an
/// OPF and a few images). Test-only: the app never writes ZIPs.
///
/// Byte-for-byte the layout `zip.rs` itself reads: a local header per
/// file, a central-directory record per file, one end-of-central-
/// directory record. The two header layouts differ — the central record
/// carries two version fields the local one does not, so a fixture that
/// copies the local header's field order parses as one short record and
/// the archive answers "central directory holds 1 entries, declares 4".
#[cfg(test)]
pub(crate) fn stored_zip_files(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    let mut offsets: Vec<u32> = Vec::with_capacity(files.len());
    for (name, data) in files {
        let name_bytes = name.as_bytes();
        let crc = crc32(data);
        let offset = out.len() as u32;
        offsets.push(offset);
        // local header
        out.extend_from_slice(b"PK\x03\x04");
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0u16.to_le_bytes()); // flags
        out.extend_from_slice(&0u16.to_le_bytes()); // method: stored
        out.extend_from_slice(&0u16.to_le_bytes()); // time
        out.extend_from_slice(&0u16.to_le_bytes()); // date
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(data);
    }
    for ((name, data), offset) in files.iter().zip(&offsets) {
        let name_bytes = name.as_bytes();
        let crc = crc32(data);
        // central-directory record
        central.extend_from_slice(b"PK\x01\x02");
        central.extend_from_slice(&20u16.to_le_bytes()); // version made by
        central.extend_from_slice(&20u16.to_le_bytes()); // version needed
        central.extend_from_slice(&0u16.to_le_bytes()); // flags
        central.extend_from_slice(&0u16.to_le_bytes()); // method
        central.extend_from_slice(&0u16.to_le_bytes()); // time
        central.extend_from_slice(&0u16.to_le_bytes()); // date
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes()); // extra
        central.extend_from_slice(&0u16.to_le_bytes()); // comment
        central.extend_from_slice(&0u16.to_le_bytes()); // disk
        central.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        central.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        central.extend_from_slice(&offset.to_le_bytes()); // local offset
        central.extend_from_slice(name_bytes);
    }
    let central_offset = out.len() as u32;
    let central_size = central.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(b"PK\x05\x06");
    out.extend_from_slice(&0u16.to_le_bytes()); // disk
    out.extend_from_slice(&0u16.to_le_bytes()); // central disk
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&central_size.to_le_bytes());
    out.extend_from_slice(&central_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // comment len
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal stored-zip builder: one file, no compression.
    fn stored_zip(name: &str, payload: &[u8]) -> Vec<u8> {
        let name_bytes = name.as_bytes();
        let crc = crc32(payload);
        let mut out = Vec::new();
        // local header
        out.extend_from_slice(b"PK\x03\x04");
        out.extend_from_slice(&20u16.to_le_bytes()); // version
        out.extend_from_slice(&0u16.to_le_bytes()); // flags
        out.extend_from_slice(&0u16.to_le_bytes()); // method: stored
        out.extend_from_slice(&0u16.to_le_bytes()); // time
        out.extend_from_slice(&0u16.to_le_bytes()); // date
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(payload);
        let local_len = out.len();
        // central directory
        out.extend_from_slice(b"PK\x01\x02");
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // flags
        out.extend_from_slice(&0u16.to_le_bytes()); // method
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra
        out.extend_from_slice(&0u16.to_le_bytes()); // comment
        out.extend_from_slice(&0u16.to_le_bytes()); // disk
        out.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        out.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        out.extend_from_slice(&0u32.to_le_bytes()); // local offset
        out.extend_from_slice(name_bytes);
        let central_len = out.len() - local_len;
        // EOCD
        out.extend_from_slice(b"PK\x05\x06");
        out.extend_from_slice(&0u16.to_le_bytes()); // disk
        out.extend_from_slice(&0u16.to_le_bytes()); // central disk
        out.extend_from_slice(&1u16.to_le_bytes()); // entries on disk
        out.extend_from_slice(&1u16.to_le_bytes()); // total entries
        out.extend_from_slice(&(central_len as u32).to_le_bytes());
        out.extend_from_slice(&(local_len as u32).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // comment len
        out
    }

    #[test]
    fn reads_a_stored_entry() {
        let zip = stored_zip("hello.txt", b"hello epub");
        let archive = Archive::parse(zip).expect("parse");
        assert_eq!(archive.read_string("hello.txt").unwrap(), "hello epub");
    }

    #[test]
    fn a_missing_entry_is_named_in_the_error() {
        let zip = stored_zip("hello.txt", b"x");
        let archive = Archive::parse(zip).unwrap();
        let err = archive.read("nope.txt").unwrap_err();
        assert!(err.to_string().contains("nope.txt"), "{err}");
    }

    #[test]
    fn a_truncated_payload_fails_the_crc() {
        // The payload is cut to six of its ten bytes while the central
        // directory (which claims ten) stays intact — the shape a real
        // truncated download has. The read must be refused rather than
        // returning garbage of a plausible length.
        let mut zip = stored_zip("a.txt", b"0123456789");
        let payload_start = 30 + 5; // local header + 5-byte name
        let tail: Vec<u8> = zip[payload_start + 10..].to_vec();
        zip.truncate(payload_start + 6);
        zip.extend_from_slice(&tail);
        let archive = Archive::parse(zip).expect("the directory still parses");
        assert!(
            archive.read("a.txt").is_err(),
            "a short payload must not read as data"
        );
    }

    #[test]
    fn a_non_zip_file_is_refused() {
        assert!(matches!(
            Archive::parse(b"not a zip at all, just prose".to_vec()),
            Err(ZipError::NotAZip)
        ));
    }

    /// The decompression-bomb probe: a central directory that declares a
    /// 4 GiB uncompressed member is refused before a byte is inflated or a
    /// buffer of that size ever exists.
    #[test]
    fn a_declared_gigabyte_member_is_refused_before_inflating() {
        let mut zip = stored_zip("bomb.txt", b"0123456789");
        // The uncompressed-size field of the central-directory record, at
        // offset 24 of the record. Declare ~4 GiB.
        //  the original patched a fixed offset (46 + name + 24) counted
        // from the file's start, which lands in the local header/payload —
        // the record is found by its signature here.
        let cd = zip
            .windows(4)
            .position(|w| w == b"PK\x01\x02")
            .expect("central directory record");
        let field = cd + 24;
        zip[field..field + 4].copy_from_slice(&0xF000_0000u32.to_le_bytes());
        let archive = Archive::parse(zip).expect("the directory still parses");
        let err = archive.read("bomb.txt").unwrap_err();
        assert!(err.to_string().contains("cap"), "{err}");
    }

    #[test]
    fn crc32_matches_the_known_vector() {
        // The canonical "123456789" CRC-32 is 0xCBF43926.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn cp437_high_bytes_decode_as_the_spec_says() {
        assert_eq!(cp437(0x41), 'A');
        assert_eq!(cp437(0x80), 'Ç');
    }
}
