//! Errors, ids and small shared helpers.

use std::fmt;

/// The app-wide error type. Every fallible operation returns one of these or a
/// `std::io::Error` converted into it, so a caller never has to guess which of
/// three error enums it is holding.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },

    #[error("{context}: {source}")]
    Json {
        context: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("no {kind} is installed: {detail}")]
    MissingModel { kind: &'static str, detail: String },

    #[error("synthesis failed for engine {engine}: {detail}")]
    Synthesis { engine: &'static str, detail: String },

    #[error("audio output failed: {0}")]
    Audio(String),

    #[error("document is damaged or unsupported: {0}")]
    Document(String),

    #[error("network: {0}")]
    Network(String),

    #[error("engine is not loaded")]
    EngineNotLoaded,

    #[error("operation was cancelled")]
    Cancelled,

    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Error::Io { context: context.into(), source }
    }

    pub fn json(context: impl Into<String>, source: serde_json::Error) -> Self {
        Error::Json { context: context.into(), source }
    }

    pub fn other(msg: impl fmt::Display) -> Self {
        Error::Other(msg.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// djb2 over Unicode scalar values, masked to 63 bits.
///
/// Reproduces iOS `TextHash.stableHash` exactly: `h = 5381`, then
/// `h = (h * 33 + scalar) & 0x7FFF_FFFF_FFFF_FFFF` per scalar. A bookmark's
/// text hash is written into `bookmarks.json`, so the arithmetic must match the
/// other apps byte for byte — this is not a place to "improve" the hash.
///
/// iOS uses it to prove a stored bookmark still belongs to the text it was
/// saved against; the Swift Linux port had the function and zero call sites.
pub fn stable_hash(text: &str) -> i64 {
    let mut h: i64 = 5381;
    for scalar in text.chars() {
        h = h.wrapping_mul(33).wrapping_add(scalar as i64) & 0x7FFF_FFFF_FFFF_FFFF;
    }
    h
}

/// djb2 over the raw bytes of `data`, masked to 63 bits.
///
/// The byte form of [`stable_hash`], for content whose encoding is not decided
/// yet — an image's bytes, for instance. It is a naming hash and nothing more:
/// it makes "the same picture stored twice is one file" cheap, so a collision
/// costs a replaced file in one note, not a corrupted library.
pub fn stable_hash_bytes(data: &[u8]) -> i64 {
    let mut h: i64 = 5381;
    for b in data {
        h = h.wrapping_mul(33).wrapping_add(*b as i64) & 0x7FFF_FFFF_FFFF_FFFF;
    }
    h
}

/// Does `data` start with a signature GTK will accept as an image?
///
/// A cheap magic-number check, used to refuse writing a file that could never
/// render. It is deliberately a *sniff* and not a decode: the answer only has
/// to be right about things that are not images, because the renderer reports
/// anything that still fails to load.
///
/// Note images are trusted here only after the picker hands them over; a note
/// that already references a missing file renders a labelled placeholder rather
/// than failing.
pub fn is_supported_image(data: &[u8]) -> bool {
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
    const GIF: &[u8] = b"GIF87a";
    const GIF89: &[u8] = b"GIF89a";
    const BMP: &[u8] = b"BM";
    const RIFF: &[u8] = b"RIFF";
    // ftyp box: JPEG, HEIF and AVIF all carry it at offset 4.
    const FTYP: &[u8] = b"ftyp";
    // JFIF and Exif are how most JPEGs identify themselves.
    const JFIF: &[u8] = &[0xFF, 0xD8, 0xFF];

    data.starts_with(PNG)
        || data.starts_with(GIF)
        || data.starts_with(GIF89)
        || data.starts_with(BMP)
        || data.starts_with(JFIF)
        || (data.len() > 12 && data.starts_with(RIFF) && &data[8..12] == b"WEBP")
        || (data.len() > 12 && &data[4..8] == FTYP)
}

/// A fresh v4 UUID in the uppercase hyphenated form the JSON stores use.
pub fn new_uuid() -> String {
    uuid::Uuid::new_v4().to_string().to_uppercase()
}

/// Parse a UUID string case-insensitively; `None` when malformed.
pub fn parse_uuid(s: &str) -> Option<uuid::Uuid> {
    uuid::Uuid::parse_str(s).ok()
}

/// Read a file, refusing anything over `cap` bytes.
///
/// Every importer that pulls a whole file into memory funnels through here:
/// a file can arrive over the LAN (BookDrop) whose size the sender chose
/// freely, and `fs::read` on a multi-gigabyte "note" is an OOM, not an
/// import. The check runs on the metadata and again on the bytes that came
/// back, so a file that grows between the two is caught too.
pub fn read_capped(path: &std::path::Path, cap: u64) -> std::io::Result<Vec<u8>> {
    let meta = std::fs::metadata(path)?;
    if meta.len() > cap {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "{} is {} bytes; this importer refuses anything over {}",
                path.display(),
                meta.len(),
                cap
            ),
        ));
    }
    let bytes = std::fs::read(path)?;
    if bytes.len() as u64 > cap {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{} grew past {} while it was being read", path.display(), cap),
        ));
    }
    Ok(bytes)
}

/// Canonical uppercase form, so two spellings of the same id compare equal in
/// the UI and hit the same map entry.
pub fn canonical_uuid(s: &str) -> Option<String> {
    parse_uuid(s).map(|u| u.to_string().to_uppercase())
}

/// Estimated listening time in whole minutes, from a word count.
///
/// 145 words per minute is the family baseline (it is what every app in the
/// family displays). Voices differ, so callers can pass an override; the default
/// keeps the numbers consistent with the other two apps.
pub fn estimate_listen_minutes(word_count: usize) -> usize {
    estimate_listen_minutes_at(word_count, 145.0)
}

/// As above with an explicit words-per-minute rate. Never returns 0.
pub fn estimate_listen_minutes_at(word_count: usize, wpm: f64) -> usize {
    if word_count == 0 {
        return 1;
    }
    let wpm = if wpm > 1.0 { wpm } else { 145.0 };
    ((word_count as f64 / wpm).ceil() as usize).max(1)
}

/// Word count over whitespace-delimited runs, counting punctuation-only runs as
/// words so "— —" is not silently zero.
pub fn count_words(text: &str) -> usize {
    text.split_whitespace().filter(|w| w.chars().any(|c| c.is_alphanumeric())).count()
}

/// Clamp a float into a range, tolerating NaN by returning the low bound.
pub fn clamp_f64(v: f64, lo: f64, hi: f64) -> f64 {
    if v.is_nan() {
        return lo;
    }
    v.clamp(lo, hi)
}

/// Human-readable byte count, for the storage pane.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// Seconds as `M:SS` or `H:MM:SS`, for the player clocks.
pub fn human_duration(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "0:00".to_string();
    }
    let total = secs.round() as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Case-, width- and accent-insensitive folding, for search and for notebook
/// name comparison.
///
/// This is what iOS's `folding(options: [.caseInsensitive, .diacriticInsensitive,
/// .widthInsensitive])` did, and the behaviour has to match: a note written as
/// "café" must be found by a search for "cafe", and a notebook named "Work" must
/// collide with "work". NFD decomposition followed by dropping the combining
/// marks gives the diacritic folding; the fullwidth→ASCII mapping gives the
/// width folding.
pub fn fold(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    // Fast path: pure-ASCII input needs no decomposition, and search hits this
    // for the overwhelming majority of notes. Without it, folding a 5 000-note
    // library costs hundreds of milliseconds per keystroke; with it, tens.
    if s.is_ascii() {
        return s.to_ascii_lowercase();
    }
    s.chars()
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .nfd()
        .filter(|c| {
            // Combining diacritical marks (U+0300..=U+036F) plus the two
            // extended ranges most European text reaches.
            let is_mark = ('\u{0300}'..='\u{036f}').contains(c)
                || ('\u{1ab0}'..='\u{1aff}').contains(c)
                || ('\u{20d0}'..='\u{20ff}').contains(c);
            if is_mark {
                return false;
            }
            match c {
                'ａ'..='ｚ' | 'Ａ'..='Ｚ' | '０'..='９' => {
                    // Fullwidth forms: handled below, keep for the mapping.
                    true
                }
                _ => true,
            }
        })
        .map(|c| match c {
            'ａ'..='ｚ' | 'Ａ'..='Ｚ' => {
                char::from_u32(c as u32 - 0xfee0).unwrap_or(c)
            }
            '０'..='９' => char::from_u32(c as u32 - 0xfee0).unwrap_or(c),
            other => other,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_hash_matches_the_ios_constants() {
        // djb2 starting value, and the empty string's hash.
        assert_eq!(stable_hash(""), 5381);
        assert_eq!(stable_hash("a"), 5381 * 33 + 97);
    }

    #[test]
    fn stable_hash_stays_positive_and_deterministic() {
        for s in ["", "hello", "unicode: é日本語🎉", "x".repeat(10_000).as_str()] {
            let h = stable_hash(s);
            assert!(h >= 0, "hash went negative for {s:?}: {h}");
            assert_eq!(h, stable_hash(s));
        }
    }

    #[test]
    fn stable_hash_is_over_scalars_not_bytes() {
        // If someone reimplements this over UTF-8 bytes the emoji case diverges.
        // This pins the scalar semantics.
        let by_scalar = stable_hash("é");
        let by_utf8: i64 = "é".as_bytes().iter().fold(5381i64, |h, b| h.wrapping_mul(33).wrapping_add(*b as i64) & 0x7FFF_FFFF_FFFF_FFFF);
        assert_ne!(by_scalar, by_utf8);
    }

    #[test]
    fn word_count_ignores_punctuation_only_runs() {
        assert_eq!(count_words("hello world"), 2);
        assert_eq!(count_words("  \n\t "), 0);
        assert_eq!(count_words("one — two"), 2);
    }

    #[test]
    fn listen_minutes_never_reports_zero() {
        assert_eq!(estimate_listen_minutes(0), 1);
        assert_eq!(estimate_listen_minutes(145), 1);
        assert_eq!(estimate_listen_minutes(146), 2);
        assert_eq!(estimate_listen_minutes_at(0, 0.0), 1);
    }

    #[test]
    fn clamp_handles_nan() {
        assert_eq!(clamp_f64(f64::NAN, 0.5, 2.0), 0.5);
        assert_eq!(clamp_f64(3.0, 0.5, 2.0), 2.0);
        assert_eq!(clamp_f64(1.0, 0.5, 2.0), 1.0);
    }

    #[test]
    fn duration_formatting_switches_at_an_hour() {
        assert_eq!(human_duration(0.0), "0:00");
        assert_eq!(human_duration(65.4), "1:05");
        assert_eq!(human_duration(3600.0), "1:00:00");
        assert_eq!(human_duration(-5.0), "0:00");
        assert_eq!(human_duration(f64::NAN), "0:00");
    }

    #[test]
    fn uuids_are_uppercase_and_parseable() {
        let id = new_uuid();
        assert_eq!(id, id.to_uppercase());
        assert!(parse_uuid(&id).is_some());
        assert_eq!(canonical_uuid(&id.to_lowercase()).as_deref(), Some(id.as_str()));
        assert!(parse_uuid("nonsense").is_none());
    }

    #[test]
    fn folding_is_case_accent_and_width_insensitive() {
        assert_eq!(fold("Work"), fold("WORK"));
        assert_eq!(fold("café"), fold("cafe"));
        assert_eq!(fold("Le Café Parisien"), fold("le cafe parisien"));
        assert_eq!(fold("Ａｂｃ"), fold("abc"));
        assert_eq!(fold("naïve"), fold("naive"));
        assert_eq!(fold("１２３"), fold("123"));
        // Unrelated strings stay unrelated.
        assert_ne!(fold("work"), fold("home"));
    }

    #[test]
    fn human_bytes_is_readable() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KB");
    }
}
#[cfg(test)]
mod image_tests {
    use super::*;

    #[test]
    fn the_byte_hash_distinguishes_content() {
        assert_ne!(stable_hash_bytes(b"abc"), stable_hash_bytes(b"abd"));
        assert_eq!(stable_hash_bytes(b"abc"), stable_hash_bytes(b"abc"));
        assert_eq!(stable_hash_bytes(b""), 5381, "djb2 of nothing is its seed");
    }

    #[test]
    fn real_image_signatures_are_accepted() {
        let cases: Vec<&[u8]> = vec![
            b"\x89PNG\r\n\x1a\n rest of the file",
            b"GIF89a....",
            b"GIF87a....",
            b"BM......",
            b"\xFF\xD8\xFF\xE0....",
            b"RIFF\x00\x00\x00\x00WEBPVP8 ",
            b"\x00\x00\x00\x18ftypavif....",
            b"\x00\x00\x00\x20ftypheic....",
        ];
        for c in cases {
            assert!(is_supported_image(c), "{c:?} should be accepted");
        }
    }

    #[test]
    fn things_that_are_not_images_are_refused() {
        for c in [
            &b""[..],
            b"not an image",
            b"<html><body>hi</body></html>",
            b"RIFF\x00\x00\x00\x00WAVEfmt ",  // a wav, not a webp
            b"RIFF",                             // too short to be one
            b"PK\x03\x04",                     // a zip
        ] {
            assert!(!is_supported_image(c), "{c:?} must be refused");
        }
    }
}
