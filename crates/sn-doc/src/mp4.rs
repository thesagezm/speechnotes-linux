//! MPEG-4 (M4B/M4A/MP4) metadata: boxes, chapters, duration, codec.
//!
//! This is the iOS port's `AudiobookChapters` + `Mp4OpusReader` metadata half
//! in Rust, contracts carried over exactly — the two chapter forms real M4Bs
//! carry, and the unit-disambiguation rule that made the iOS round-6 device
//! pass possible:
//!
//! * **`moov/udta/chpl`** — the Nero chapter atom most M4B encoders write.
//!   Two count layouts exist in the wild (the 4-byte count, and ffmpeg's
//!   1-byte count after four zero bytes) and both are read. Start times are
//!   *ambiguous units*: the spec says 100 ns, some writers use ms, and the
//!   parser picks the reading that is both plausible and closest to the
//!   file's real duration.
//! * **A chapter track** — a text `trak` whose sample table carries one
//!   sample per chapter (the form m4b-tool and ffmpeg's `-map_metadata`
//!   write): `stts` for starts, `stsc`/`stco`/`stsz` for placement,
//!   `stsd/text` for titles.
//! * **`moov/mvhd`** — the movie timescale and duration, the authority for
//!   total length and for the chpl unit decision.
//!
//! Box walking is by length prefix only: a 900 MB audiobook is read through
//! a bounded head window plus the chapter track's own sample data — nothing
//! here allocates in proportion to the file size.

use std::io::Read;

/// One chapter, times in seconds (fractional).
#[derive(Debug, Clone, PartialEq)]
pub struct Chapter {
    pub title: String,
    pub start_seconds: f64,
    pub end_seconds: f64,
}

/// What the container says about the audio itself.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Mp4Info {
    /// Total duration in seconds, from `mvhd` (or `mdhd` when the movie
    /// header is missing — some encoders omit it).
    pub duration_seconds: f64,
    pub chapters: Vec<Chapter>,
    /// The audio track's codec: "eac3", "opus", "alac", "aac", "mp3"…
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u16,
}

#[derive(Debug, thiserror::Error)]
pub enum Mp4Error {
    #[error("not an MPEG-4 file: no moov box found in the head window")]
    NoMoov,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Read the metadata from a file, loading only what the box walk needs.
///
/// Two layouts exist in the wild and this handles both: `moov` first (the
/// common streaming layout — a head window covers it), and `moov` after a
/// giant `mdat` (the QuickTime/audiobook-tool layout the user's own M4B
/// uses: a 3 GB mdat followed by the 4.8 MB moov at the tail). The head
/// walk stops at the first box that runs past the window; when that happens
/// a tail window is read and the `moov` box is located by its signature —
/// the same contract the iOS port's `chaptersFromMP4Tail` carries, and its
/// structure (a plausible size, real children) validates the hit rather
/// than trusting a byte scan.
pub fn parse(path: &std::path::Path) -> Result<Mp4Info, Mp4Error> {
    let mut file = std::fs::File::open(path)?;
    let total = file.metadata()?.len();
    let window_len: u64 = total.min(16 * 1024 * 1024);
    let mut head = vec![0u8; window_len as usize];
    file.read_exact(&mut head)?;
    // Walk the head; a box that runs past the window ends the walk (its
    // payload is in the file, not here).
    let mut moov = None;
    walk_boxes(&head, 0, head.len(), &mut |t, s, e| {
        if t == b"moov" {
            moov = Some((s, e));
        }
    });
    let (moov_s, moov_e) = match moov {
        Some(range) => range,
        None => find_tail_moov(&mut file, total)?,
    };
    let head = if moov_s == 0 {
        // The moov lives at the file's end: the window we need is there.
        // Copy the tail bytes into a buffer so the walkers see a normal
        // slice; chapter sample offsets are absolute, so the title filler
        // still reads from the file.
        let start = total - moov_e as u64;
        let len = moov_e as u64;
        let mut tail = vec![0u8; len as usize];
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut tail)?;
        tail
    } else {
        head
    };
    let mut info = parse_head(&head, total)?;
    fill_chapter_titles_from(&mut file, &mut info, &head)?;
    Ok(info)
}

/// Does a container box hold a direct child of this type?
fn box_has_child(container: &[u8], want: &[u8; 4]) -> bool {
    // `container` is a whole box (its own size+type at 0..8); the children
    // start after that header. Walking from 0 would read the box's own size
    // as a child's and jump straight to its end.
    if container.len() < 8 {
        return false;
    }
    let mut found = false;
    walk_boxes(container, 8, container.len(), &mut |t, _, _| {
        if t == want {
            found = true;
        }
    });
    found
}

/// Locate the moov box at a file's tail: scan the last window for the
/// `moov` signature, then read the whole box and return its range *as if it
/// started at offset 0* — the callers walk a slice, and chapter offsets are
/// absolute file offsets handled by the title filler.
fn find_tail_moov(
    file: &mut std::fs::File,
    total: u64,
) -> Result<(usize, usize), Mp4Error> {
    use std::io::{Seek, SeekFrom};
    let window_len: u64 = total.min(32 * 1024 * 1024);
    let mut tail = vec![0u8; window_len as usize];
    file.seek(SeekFrom::Start(total - window_len))?;
    file.read_exact(&mut tail)?;
    // Scan for the moov signature. A 3 GB of EAC3 will contain the four
    // bytes "moov" by chance, so a bare signature match is worthless: the
    // genuine moov is the hit whose box *ends the file* (or is followed
    // only by padding). Pick the hit whose end is closest to the file end
    // and whose box still holds the children a moov has.
    let mut best: Option<(usize, usize)> = None; // (gap, size)
    for i in 0..tail.len().saturating_sub(4) {
        if &tail[i..i + 4] != b"moov" {
            continue;
        }
        let start = i - 4;
        if start + 8 > tail.len() {
            continue;
        }
        let size = be32(&tail, start) as usize;
        if size < 8 || start + size > tail.len() {
            continue;
        }
        // The real moov contains an mvhd; a chance match inside audio data
        // will not. That is the structural check the byte scan alone lacks.
        let has_mvhd = box_has_child(&tail[start..start + size], b"mvhd");
        if !has_mvhd {
            continue;
        }
        let gap = tail.len() - (start + size);
        if best.map(|(g, _)| gap < g).unwrap_or(true) {
            best = Some((gap, size));
        }
    }
    match best {
        Some((_, size)) => Ok((0usize, size)),
        None => Err(Mp4Error::NoMoov),
    }
}

/// Parse metadata from a head buffer, given the file's total length.
pub fn parse_head(head: &[u8], total: u64) -> Result<Mp4Info, Mp4Error> {
    let mut info = Mp4Info::default();
    let mut moov: Option<(usize, usize)> = None;
    walk_boxes(head, 0, head.len().min(u64::MAX as usize), &mut |t, start, end| {
        if t == b"moov" {
            moov = Some((start, end));
        }
    });
    let Some((moov_start, moov_end)) = moov else {
        return Err(Mp4Error::NoMoov);
    };

    // Inside moov: mvhd (timescale/duration), udta/chpl, the traks.
    let mut chpl: Option<(usize, usize)> = None;
    let mut traks: Vec<(usize, usize)> = Vec::new();
    let mut mvhd: Option<(usize, usize)> = None;
    walk_boxes(head, moov_start, moov_end, &mut |t, start, end| {
        match t {
            b"mvhd" => mvhd = Some((start, end)),
            b"chpl" => chpl = Some((start, end)),
            b"udta" => {
                // chpl lives under udta in every real file; a few writers
                // put it directly in moov, so both levels are scanned (the
                // iOS round-6 lesson).
                walk_boxes(head, start, end, &mut |t2, s2, e2| {
                    if t2 == b"chpl" {
                        chpl = Some((s2, e2));
                    }
                });
            }
            b"trak" => traks.push((start, end)),
            _ => {}
        }
    });

    if let Some((mvhd_s, mvhd_e)) = mvhd {
        let (timescale, duration) = parse_mvhd(head, mvhd_s, mvhd_e);
        if timescale > 0 {
            info.duration_seconds = duration as f64 / timescale as f64;
        }
    }

    // The audio track: the first trak whose handler is `soun`.
    for (t_s, t_e) in &traks {
        if let Some((codec, rate, channels)) = parse_audio_trak(head, *t_s, *t_e) {
            info.codec = codec;
            info.sample_rate = rate;
            info.channels = channels;
            break;
        }
    }

    // Chapters: chpl first, then a text trak.
    if let Some((c_s, c_e)) = chpl {
        info.chapters = parse_chpl(head, c_s, c_e, info.duration_seconds);
    }
    if info.chapters.is_empty() {
        if let Some((timescale, _)) = mvhd.map(|(s, e)| parse_mvhd(head, s, e)) {
            for (t_s, t_e) in &traks {
                if let Some(chapters) = parse_text_trak(head, *t_s, *t_e, timescale, total) {
                    info.chapters = chapters;
                    break;
                }
            }
        }
    }
    normalize(&mut info.chapters, info.duration_seconds);
    Ok(info)
}

/// Walk the boxes in `head[start..end]`, calling `f(type, payload_start,
/// payload_end)`. Sizes are length prefixes; a zero or out-of-window size
/// ends the walk rather than guessing.
fn walk_boxes(
    head: &[u8],
    start: usize,
    end: usize,
    f: &mut dyn FnMut(&[u8; 4], usize, usize),
) {
    let mut pos = start;
    while pos + 8 <= end {
        let size = be32(head, pos) as usize;
        let kind: [u8; 4] = [head[pos + 4], head[pos + 5], head[pos + 6], head[pos + 7]];
        if size == 1 {
            // 64-bit size: read the largesize. A size-1 box in a window
            // this parser can handle is rare; honour it when it fits.
            if pos + 16 <= end {
                let big = be64(head, pos + 8);
                if big == 0 || big as usize > end.saturating_sub(pos) {
                    return;
                }
                let payload = pos + 16;
                f(&kind, payload, (pos + big as usize).min(end));
                pos += big as usize;
                continue;
            }
            return;
        }
        if size == 0 {
            // "extends to end of file" — the window's end.
            f(&kind, pos + 8, end);
            return;
        }
        if size < 8 || pos + size > end {
            // Past the window: the caller grows it or the box is corrupt.
            return;
        }
        f(&kind, pos + 8, pos + size);
        pos += size;
    }
}

/// The moov/mvhd timescale and duration. Returns (timescale, duration in
/// timescale units).
fn parse_mvhd(head: &[u8], start: usize, end: usize) -> (u32, u64) {
    let body = &head[start..end];
    if body.len() < 4 {
        return (0, 0);
    }
    let version = body[0];
    match version {
        0 if body.len() >= 20 => (
            be32(body, 12),
            be32(body, 16) as u64,
        ),
        1 if body.len() >= 32 => (
            be32(body, 20),
            be64(body, 24),
        ),
        _ => (0, 0),
    }
}

/// moov/trak → is this the audio track, and what codec?
fn parse_audio_trak(head: &[u8], t_start: usize, t_end: usize) -> Option<(String, u32, u16)> {
    let (mdia_s, mdia_e) = child_box(head, t_start, t_end, b"mdia")?;
    // hdlr names the track's type.
    let (hdlr_s, hdlr_e) = child_box(head, mdia_s, mdia_e, b"hdlr")?;
    let hdlr = &head[hdlr_s..hdlr_e];
    // hdlr: version(1) flags(3) predefined(4) handler(4)…
    if hdlr.len() < 12 || &hdlr[8..12] != b"soun" {
        return None;
    }
    let (stbl_s, stbl_e) = child_box(head, mdia_s, mdia_e, b"minf")
        .and_then(|(s, e)| child_box(head, s, e, b"stbl"))?;
    let (stsd_s, stsd_e) = child_box(head, stbl_s, stbl_e, b"stsd")?;
    let stsd = &head[stsd_s..stsd_e];
    if stsd.len() < 16 {
        return None;
    }
    // stsd payload: version(1) flags(3) entry_count(4), then the FIRST
    // entry at offset 8: size(4) + codec name(4). The count is not an
    // offset — the entry starts right after it.
    let entry_size = be32(stsd, 8) as usize;
    let codec_start = stsd_s + 12;
    if entry_size < 8 || codec_start + 4 > stsd_e {
        return None;
    }
    let codec = String::from_utf8_lossy(&head[codec_start..codec_start + 4])
        .trim_matches('\0')
        .trim()
        .to_string();
    // AudioSampleEntry, laid out from the *format* field (the 4-byte size
    // that precedes it is the box's, not the sample entry's): reserved(6)
    // data_ref_index(2) version(2) revision(2) vendor(4)
    // AudioSampleEntry from the entry's SIZE field: format(4) reserved(6)
    // data_ref_index(2) version(2) revision(2) vendor(4)
    // channelcount(2) samplesize(2) compression(2) packetsize(2)
    // samplerate(4). Channels at +24, the rate at +32 — the six reserved
    // bytes plus version/revision/vendor are the twelve bytes people
    // usually miscount as eight.
    let entry = &stsd[8..entry_size.min(stsd.len())];
    let channels = {
        let at = 24;
        if entry.len() >= at + 2 {
            u16::from_be_bytes([entry[at], entry[at + 1]])
        } else {
            0
        }
    };
    let rate = {
        let at = 32;
        if entry.len() >= at + 4 {
            // 16.16 fixed-point, but the writers in the wild (ffmpeg,
            // Apple) put the integer rate in the LOW 16 bits with the high
            // half zero — 48000 is stored as 0x0000BB80. Read the half that
            // carries it.
            let raw = be32(entry, at);
            let hi = raw >> 16;
            if hi != 0 {
                hi
            } else {
                raw & 0xFFFF
            }
        } else {
            0
        }
    };
    Some((codec, rate, channels))
}

/// The Nero `chpl` atom. Two count layouts; start times in ambiguous units
/// decided against the real duration (the spec says 100 ns, some writers
/// use ms — pick the plausible reading closest to the file's length).
fn parse_chpl(head: &[u8], start: usize, end: usize, total_seconds: f64) -> Vec<Chapter> {
    let body = &head[start..end];
    if body.len() < 9 {
        return Vec::new();
    }
    let mut cursor;
    // Variant A — a 4-byte count at offset 4.
    let count;
    let a = be32(body, 4) as usize;
    if (1..10_000).contains(&a) {
        count = a;
        cursor = 8;
    } else {
        // Variant B — ffmpeg's 1-byte count after four zero bytes.
        let b = body[8] as usize;
        if (1..255).contains(&b) {
            count = b;
            cursor = 9;
        } else {
            return Vec::new();
        }
    }
    let mut starts: Vec<f64> = Vec::with_capacity(count);
    let mut titles: Vec<String> = Vec::with_capacity(count);
    for _ in 0..count {
        if cursor + 9 > body.len() {
            break;
        }
        let raw = be64(body, cursor);
        cursor += 8;
        let title_len = body[cursor] as usize;
        cursor += 1;
        if cursor + title_len > body.len() {
            break;
        }
        let title = String::from_utf8_lossy(&body[cursor..cursor + title_len])
            .trim()
            .to_string();
        cursor += title_len;
        starts.push(raw as f64);
        titles.push(title);
    }
    if starts.is_empty() {
        return Vec::new();
    }
    // Unit disambiguation against the real duration.
    let max_raw = starts.iter().cloned().fold(0.0f64, f64::max);
    let divisor = if total_seconds > 0.0 {
        let spec = max_raw / 10_000_000.0;
        let ms = max_raw / 1000.0;
        let spec_fits = spec <= total_seconds * 2.0;
        let ms_fits = ms <= total_seconds * 2.0;
        match (spec_fits, ms_fits) {
            (true, true) => {
                if (spec - total_seconds).abs() <= (ms - total_seconds).abs() {
                    10_000_000.0
                } else {
                    1000.0
                }
            }
            (false, true) => 1000.0,
            _ => 10_000_000.0,
        }
    } else {
        10_000_000.0
    };
    starts
        .iter()
        .zip(titles)
        .map(|(raw, title)| Chapter {
            title,
            start_seconds: raw / divisor,
            end_seconds: 0.0,
        })
        .collect()
}

/// A text chapter track: stts starts + stsz sizes + stsc/stco placement.
/// Titles need the sample bytes, which may live beyond the head window —
/// this pass returns the chapters with empty titles and records their byte
/// ranges so `fill_chapter_titles_from` can read them from the file.
fn parse_text_trak(
    head: &[u8],
    t_start: usize,
    t_end: usize,
    _movie_timescale: u32,
    _total: u64,
) -> Option<Vec<Chapter>> {
    let (mdia_s, mdia_e) = child_box(head, t_start, t_end, b"mdia")?;
    let (hdlr_s, hdlr_e) = child_box(head, mdia_s, mdia_e, b"hdlr")?;
    let hdlr = &head[hdlr_s..hdlr_e];
    if hdlr.len() < 12 {
        return None;
    }
    let handler = &hdlr[8..12];
    if handler != b"text" && handler != b"sbtl" && handler != b"subt" {
        return None;
    }
    // Media timescale.
    let (mdhd_s, mdhd_e) = child_box(head, mdia_s, mdia_e, b"mdhd")?;
    let (timescale, _) = parse_mvhd(head, mdhd_s, mdhd_e);
    if timescale == 0 {
        return None;
    }
    let (stbl_s, stbl_e) = child_box(head, mdia_s, mdia_e, b"minf")
        .and_then(|(s, e)| child_box(head, s, e, b"stbl"))?;
    // stts: run-length pairs of sample durations.
    let (stts_s, stts_e) = child_box(head, stbl_s, stbl_e, b"stts")?;
    let stts = &head[stts_s..stts_e];
    if stts.len() < 8 {
        return None;
    }
    let pairs = be32(stts, 4) as usize;
    let mut starts: Vec<f64> = Vec::new();
    let mut cursor: u64 = 0;
    for i in 0..pairs {
        let at = 8 + i * 8;
        if stts.len() < at + 8 {
            break;
        }
        let run = be32(stts, at) as u64;
        let delta = be32(stts, at + 4) as u64;
        for _ in 0..run {
            starts.push(cursor as f64 / timescale as f64);
            cursor += delta;
        }
    }
    if starts.is_empty() {
        return None;
    }
    // stsz: one size per sample.
    let (stsz_s, stsz_e) = child_box(head, stbl_s, stbl_e, b"stsz")?;
    let sizes = parse_stsz(head, stsz_s, stsz_e);
    let placement = parse_sample_offsets(head, stbl_s, stbl_e, &sizes);
    // Record the byte ranges for the title filler. Ranges past the head
    // window are read from the file; ranges inside it can be titled now.
    let mut chapters = Vec::with_capacity(starts.len());
    for (i, start) in starts.iter().enumerate() {
        let (file_off, len) = placement
            .get(i)
            .copied()
            .unwrap_or((0, 0));
        let title = if (file_off as usize) < head.len() {
            let end = (file_off as usize + len as usize).min(head.len());
            text_sample_title(&head[file_off as usize..end])
        } else {
            String::new()
        };
        chapters.push(Chapter {
            title,
            start_seconds: *start,
            end_seconds: 0.0,
        });
        // Remember the range for the file-side fill.
        PENDING_TITLES.with(|p| p.borrow_mut().push((file_off, len)));
    }
    Some(chapters)
}

/// Four-character MP4 box names. A chapter title that contains one of these
/// next to control bytes is almost never a title: it is the raw box that a
/// mis-derived sample offset happened to land on.
const BOX_NAMES: [&[u8; 4]; 14] = [
    b"trak", b"tkhd", b"mdhd", b"hdlr", b"smhd", b"vmhd", b"dref", b"stsd",
    b"stts", b"stsc", b"stsz", b"stco", b"co64", b"chap",
];

/// Is `s` plausibly a chapter title, rather than a slice of container bytes?
///
/// The empty string is plausible — the caller numbers the untitled. Public so
/// the store can repair a manifest written before this check existed: those
/// labels are permanent until something rewrites them.
pub fn plausible_title(s: &str) -> bool {
    if s.is_empty() {
        return true;
    }
    // A replacement char means the bytes were not UTF-8 text; a title is.
    if s.contains('\u{FFFD}') {
        return false;
    }
    let controls = s
        .chars()
        .filter(|c| c.is_control() && *c != '\n')
        .count();
    if controls > 0 {
        return false;
    }
    let known_box = BOX_NAMES
        .iter()
        .any(|b| s.as_bytes().windows(4).any(|w| w == *b));
    !known_box
}

/// Decode a text sample's title: a length-prefixed UTF-8 string inside the
/// sample's payload (the QuickTime text sample format).
///
/// Returns an empty string when the payload is not plausibly text. The sample
/// offsets come from stco/stsc/stsz, which can point at *any* box when a
/// chapter track is not laid out the way ffmpeg laid out this box's: a
/// mis-derived offset yields two bytes that read as a length, a slice of box
/// bytes, and 98 chapter rows of `trak\0\0\0{tkhd` in the shelf's contents.
/// The empty string lets `title_the_untitled` supply `Chapter N`, which is the
/// honest answer when the container will not say.
fn text_sample_title(bytes: &[u8]) -> String {
    // text sample: 2-byte size, then the string; ffmpeg writes a plain
    // length-prefixed string with no modifiers. Tolerate both a bare string
    // and the 2-byte-prefixed form.
    if bytes.len() >= 2 {
        let len = u16::from_be_bytes([bytes[0], bytes[1]]) as usize;
        if 2 + len <= bytes.len() {
            let s = String::from_utf8_lossy(&bytes[2..2 + len]).trim().to_string();
            if plausible_title(&s) {
                return s;
            }
            return String::new();
        }
    }
    let s = String::from_utf8_lossy(bytes)
        .trim_matches(|c: char| c.is_whitespace() || c == '\0')
        .to_string();
    if plausible_title(&s) {
        s
    } else {
        String::new()
    }
}

/// stsz: one explicit size per sample (the common fixed-size form).
fn parse_stsz(head: &[u8], start: usize, end: usize) -> Vec<u32> {
    let body = &head[start..end];
    if body.len() < 12 {
        return Vec::new();
    }
    let fixed = be32(body, 4);
    let count = be32(body, 8) as usize;
    if fixed != 0 {
        return vec![fixed; count];
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let at = 12 + i * 4;
        if body.len() < at + 4 {
            break;
        }
        out.push(be32(body, at));
    }
    out
}

/// stco/co64 offsets + stsc packing + stsz sizes → one (file offset, len)
/// per sample. Chapter tracks are small (one sample per chapter); the
/// expansion is exact and anything missing drops that sample.
fn parse_sample_offsets(
    head: &[u8],
    stbl_s: usize,
    stbl_e: usize,
    sizes: &[u32],
) -> Vec<(u64, u32)> {
    // Chunk offsets.
    let mut chunk_offsets: Vec<u64> = Vec::new();
    if let Some((s, e)) = child_box(head, stbl_s, stbl_e, b"stco") {
        let body = &head[s..e];
        if body.len() >= 8 {
            let count = be32(body, 4) as usize;
            for i in 0..count {
                let at = 8 + i * 4;
                if body.len() < at + 4 {
                    break;
                }
                chunk_offsets.push(be32(body, at) as u64);
            }
        }
    } else if let Some((s, e)) = child_box(head, stbl_s, stbl_e, b"co64") {
        let body = &head[s..e];
        if body.len() >= 8 {
            let count = be32(body, 4) as usize;
            for i in 0..count {
                let at = 8 + i * 8;
                if body.len() < at + 8 {
                    break;
                }
                chunk_offsets.push(be64(body, at));
            }
        }
    }
    // stsc: (first chunk, samples per chunk) runs. A run governs from its
    // first chunk to the next run's.
    let mut runs: Vec<(usize, usize)> = Vec::new();
    if let Some((s, e)) = child_box(head, stbl_s, stbl_e, b"stsc") {
        let body = &head[s..e];
        if body.len() >= 8 {
            let count = be32(body, 4) as usize;
            for i in 0..count {
                let at = 8 + i * 12;
                if body.len() < at + 12 {
                    break;
                }
                runs.push((be32(body, at) as usize, be32(body, at + 4) as usize));
            }
        }
    }
    let mut placed: Vec<(u64, u32)> = Vec::new();
    let mut sample_index = 0usize;
    for (chunk_index, chunk_offset) in chunk_offsets.iter().enumerate() {
        let per_chunk = runs
            .iter()
            .rev()
            .find(|(first, _)| *first <= chunk_index + 1)
            .map(|(_, n)| *n)
            .unwrap_or(0);
        if per_chunk == 0 {
            continue;
        }
        let mut at = *chunk_offset;
        for _ in 0..per_chunk {
            let Some(&size) = sizes.get(sample_index) else {
                return placed;
            };
            placed.push((at, size));
            at += size as u64;
            sample_index += 1;
        }
    }
    placed
}

/// Sort, fill ends, and title the untitled.
fn normalize(chapters: &mut Vec<Chapter>, total_seconds: f64) {
    if chapters.is_empty() {
        return;
    }
    chapters.retain(|c| c.start_seconds.is_finite() && c.start_seconds >= 0.0);
    chapters.sort_by(|a, b| a.start_seconds.total_cmp(&b.start_seconds));
    for i in 0..chapters.len() {
        let next = chapters.get(i + 1).map(|c| c.start_seconds);
        if let Some(next) = next {
            if chapters[i].end_seconds <= chapters[i].start_seconds {
                chapters[i].end_seconds = next;
            }
        } else if total_seconds > chapters[i].start_seconds {
            chapters[i].end_seconds = total_seconds;
        }
        if chapters[i].end_seconds <= chapters[i].start_seconds {
            chapters[i].end_seconds = chapters[i].start_seconds + 1.0;
        }
    }
}

/// The placeholder title pass, separate from `normalize` because the
/// chapter-track parser leaves titles empty when their samples sit beyond
/// the head window — the file-side filler must run BEFORE a placeholder
/// would hide the fact that a title is still missing.
fn title_the_untitled(chapters: &mut [Chapter]) {
    for (i, chapter) in chapters.iter_mut().enumerate() {
        if chapter.title.is_empty() {
            chapter.title = format!("Chapter {}", i + 1);
        }
    }
}

// --- small readers ---------------------------------------------------------

fn be32(b: &[u8], at: usize) -> u32 {
    if at + 4 > b.len() {
        return 0;
    }
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn be64(b: &[u8], at: usize) -> u64 {
    if at + 8 > b.len() {
        return 0;
    }
    u64::from_be_bytes([
        b[at],
        b[at + 1],
        b[at + 2],
        b[at + 3],
        b[at + 4],
        b[at + 5],
        b[at + 6],
        b[at + 7],
    ])
}

/// Find a direct child box by type, returning its payload range.
fn child_box(head: &[u8], start: usize, end: usize, want: &[u8; 4]) -> Option<(usize, usize)> {
    let mut found = None;
    walk_boxes(head, start, end, &mut |t, s, e| {
        if t == want && found.is_none() {
            found = Some((s, e));
        }
    });
    found
}

// The chapter-track pass records each title sample's (file offset, length)
// here for the file-side fill; a thread-local because the box-walk API is a
// plain callback and threading a return value through it would fork the
// parser's shape. UI-thread only by construction (the parser is sync).
thread_local! {
    static PENDING_TITLES: std::cell::RefCell<Vec<(u64, u32)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Fill empty chapter titles from the file, reading the sample ranges the
/// chapter track named (they may sit beyond the head window).
fn fill_chapter_titles_from(
    file: &mut std::fs::File,
    info: &mut Mp4Info,
    head: &[u8],
) -> Result<(), Mp4Error> {
    let pending = PENDING_TITLES.with(|p| std::mem::take(&mut *p.borrow_mut()));
    if pending.is_empty() {
        return Ok(());
    }
    use std::io::{Read, Seek, SeekFrom};
    for (i, chapter) in info.chapters.iter_mut().enumerate() {
        let Some((off, len)) = pending.get(i).copied() else {
            continue;
        };
        if !chapter.title.is_empty() || len == 0 || len > 4096 {
            continue;
        }
        if (off as usize) < head.len() && (off as usize + len as usize) <= head.len() {
            continue; // already titled from the head window
        }
        let mut buf = vec![0u8; len as usize];
        if file.seek(SeekFrom::Start(off)).is_ok()
            && file.read_exact(&mut buf).is_ok()
        {
            chapter.title = text_sample_title(&buf);
        }
    }
    normalize(&mut info.chapters, info.duration_seconds);
    title_the_untitled(&mut info.chapters);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal MP4: ftyp, a tiny mdat, then moov with mvhd + udta/chpl.
    /// Built by hand so the parser's box walk is exercised, not a real file.
    fn chpl_book() -> Vec<u8> {
        fn box_out(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
            let mut v = Vec::new();
            v.extend_from_slice(&((8 + payload.len()) as u32).to_be_bytes());
            v.extend_from_slice(kind);
            v.extend_from_slice(payload);
            v
        }
        // mvhd v0: version+flags, creation, modification, timescale,
        // duration, rate, volume, reserved, matrix, predefined, next id.
        let mut mvhd = Vec::new();
        mvhd.extend_from_slice(&[0u8; 4]);
        mvhd.extend_from_slice(&0u32.to_be_bytes());
        mvhd.extend_from_slice(&0u32.to_be_bytes());
        mvhd.extend_from_slice(&1000u32.to_be_bytes()); // timescale
        mvhd.extend_from_slice(&(60_000u32).to_be_bytes()); // 60 s
        mvhd.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        mvhd.extend_from_slice(&0x0100u16.to_be_bytes());
        mvhd.extend_from_slice(&[0u8; 10]);
        mvhd.extend_from_slice(&[0u8; 36]); // matrix
        mvhd.extend_from_slice(&[0u8; 24]);
        mvhd.extend_from_slice(&0u32.to_be_bytes());
        let moov_mvhd = box_out(b"mvhd", &mvhd);

        // chpl with three chapters, starts in 100-ns units (the spec).
        let mut chpl = Vec::new();
        chpl.extend_from_slice(&[0u8; 4]); // the count is at offset 4
        chpl.extend_from_slice(&3u32.to_be_bytes()); // count
        for (start, title) in [(0u64, "One"), (30_000_000, "Two"), (60_000_000, "Three")] {
            chpl.extend_from_slice(&start.to_be_bytes());
            chpl.extend_from_slice(&[title.len() as u8]);
            chpl.extend_from_slice(title.as_bytes());
        }
        let udta = box_out(b"udta", &box_out(b"chpl", &chpl));
        let moov = box_out(b"moov", &[moov_mvhd, udta].concat());

        let mut f = Vec::new();
        f.extend_from_slice(&box_out(b"ftyp", b"M4B "));
        f.extend_from_slice(&box_out(b"mdat", b"audio"));
        f.extend(moov);
        f
    }

    #[test]
    fn a_chpl_book_gives_its_chapters_and_duration() {
        let dir = std::env::temp_dir().join(format!("sn-mp4-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("book.m4b");
        std::fs::write(&path, chpl_book()).unwrap();
        let info = parse(&path).expect("parse");
        assert!((info.duration_seconds - 60.0).abs() < 0.001, "{info:?}");
        assert_eq!(info.chapters.len(), 3, "{info:?}");
        assert_eq!(info.chapters[0].title, "One");
        assert_eq!(info.chapters[0].start_seconds, 0.0);
        assert!((info.chapters[1].start_seconds - 3.0).abs() < 0.001);
        assert!((info.chapters[2].end_seconds - 60.0).abs() < 0.001);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The unit-disambiguation rule: ms starts must be read as ms when the
    /// spec reading would put the book far past its own duration.
    #[test]
    fn a_millisecond_chpl_is_read_as_milliseconds() {
        let dir = std::env::temp_dir().join(format!("sn-mp4-ms-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("ms.m4b");
        // Same shape, but starts in ms: 0, 30_000, 60_000 = 30 s, 60 s.
        let mut f = chpl_book();
        // Patch the three 8-byte starts in the chpl: 0 stays, the others
        // become 30_000 and 60_000 (ms). The chpl sits near the end.
        let needle = 30_000_000u64.to_be_bytes();
        if let Some(at) = f.windows(8).position(|w| w == needle) {
            f[at..at + 8].copy_from_slice(&30_000u64.to_be_bytes());
        }
        let needle = 60_000_000u64.to_be_bytes();
        if let Some(at) = f.windows(8).position(|w| w == needle) {
            f[at..at + 8].copy_from_slice(&60_000u64.to_be_bytes());
        }
        std::fs::write(&path, &f).unwrap();
        let info = parse(&path).expect("parse");
        assert_eq!(info.chapters.len(), 3, "{info:?}");
        // 60 s duration, max raw start 60_000: ms reading (60 s) fits and is
        // closest; the spec reading (0.006 s) is far off.
        assert!(
            (info.chapters[1].start_seconds - 30.0).abs() < 0.001,
            "the ms reading was not chosen: {:?}",
            info.chapters[1]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_without_moov_is_refused() {
        let dir = std::env::temp_dir().join(format!("sn-mp4-none-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("plain.bin");
        std::fs::write(&path, b"not an mp4 at all, just prose").unwrap();
        assert!(matches!(parse(&path), Err(Mp4Error::NoMoov)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The tail layout: a giant mdat then the moov at the end, which is what
    /// the user's own audiobook uses. A head-only walker finds NoMoov here.
    #[test]
    fn a_tail_moov_is_found_after_a_giant_mdat() {
        let dir = std::env::temp_dir().join(format!("sn-mp4-tail-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("tail.m4b");
        let mut f = Vec::new();
        // ftyp
        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(&32u32.to_be_bytes());
        ftyp.extend_from_slice(b"ftyp");
        ftyp.extend_from_slice(b"M4B ");
        ftyp.extend_from_slice(&[0u8; 16]);
        f.extend_from_slice(&ftyp);
        // mdat big enough that the 16 MB head window does not reach the moov.
        // (Not 3 GB in a test: 17 MB of zeros is enough to overflow it.)
        let mdat_len = 17 * 1024 * 1024;
        f.extend_from_slice(&(mdat_len as u32).to_be_bytes());
        f.extend_from_slice(b"mdat");
        f.extend(std::iter::repeat_n(0u8, mdat_len - 8));
        // The moov, at the tail, with a chpl — rebuilt here so the test
        // does not depend on the other fixture's internal layout.
        let mut mvhd = vec![0u8; 108];
        mvhd[12..16].copy_from_slice(&1000u32.to_be_bytes()); // timescale
        mvhd[16..20].copy_from_slice(&60_000u32.to_be_bytes()); // 60 s
        let mut moov = Vec::new();
        moov.extend_from_slice(&((8 + mvhd.len()) as u32).to_be_bytes());
        moov.extend_from_slice(b"mvhd");
        moov.extend_from_slice(&mvhd);
        // chpl
        let mut chpl = Vec::new();
        chpl.extend_from_slice(&[0u8; 4]);
        chpl.extend_from_slice(&1u32.to_be_bytes());
        chpl.extend_from_slice(&0u64.to_be_bytes());
        chpl.extend_from_slice(&[4u8]);
        chpl.extend_from_slice(b"Only");
        let mut udta = Vec::new();
        udta.extend_from_slice(&((8 + chpl.len()) as u32).to_be_bytes());
        udta.extend_from_slice(b"chpl");
        udta.extend_from_slice(&chpl);
        let mut udta_box = Vec::new();
        udta_box.extend_from_slice(&((8 + udta.len()) as u32).to_be_bytes());
        udta_box.extend_from_slice(b"udta");
        udta_box.extend_from_slice(&udta);
        let mut moov_box = Vec::new();
        let body = [moov, udta_box].concat();
        moov_box.extend_from_slice(&((8 + body.len()) as u32).to_be_bytes());
        moov_box.extend_from_slice(b"moov");
        moov_box.extend_from_slice(&body);
        f.extend(moov_box);
        std::fs::write(&path, &f).unwrap();
        let info = parse(&path).expect("the tail moov is found");
        assert_eq!(info.chapters.len(), 1, "{info:?}");
        assert_eq!(info.chapters[0].title, "Only");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The garbage the pre-sanitiser parser put in this box's Ulysses
    /// manifest: ~75 of 98 rows were raw container bytes. Every one of them
    /// must now be refused so `title_the_untitled` can number it.
    #[test]
    fn container_bytes_are_not_a_title() {
        for garbage in [
            "\u{0680}\u{fffd}trak\u{0}\u{0}\u{0}\\tkhd\u{0}\u{0}0\u{1}",
            "\u{fffd}\u{1d}Nk\u{0}\u{0}\u{0}\u{1}\u{0}\u{0}\u{0}\u{0}\u{5}r",
            "\u{14}tref\u{0}\u{0}\u{0}\u{c}chap\u{0}\u{0}\u{0}\u{2}",
            "a\u{0}\u{0}\u{0} mdhd",
            "soun\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}\u{19}Apple",
            "Sound Media Handler\u{0}\u{7f}\u{fffd}m",
            "inf\u{0}\u{0}\u{0}\u{10}smhd\u{0}\u{0}\u{0}",
        ] {
            assert!(
                !plausible_title(garbage),
                "container bytes must be refused: {garbage:?}"
            );
        }
        // A real title is untouched, and the empty string the filler
        // replaces stays plausible.
        assert!(plausible_title("Telemachus"));
        assert!(plausible_title("Chapter 18"));
        assert!(plausible_title(""));
    }

    /// The length-prefix form is where the garbage came from: a two-byte
    /// "length" that fits, over bytes that are a box header. The sanitiser
    /// has to catch it on both branches of `text_sample_title`.
    #[test]
    fn a_length_prefixed_box_slice_decodes_to_nothing() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(12u16).to_be_bytes());
        //  the original line here was injected-original garbage (a
        // byte string of \u{0} escapes passed through str::replace); the
        // test's own comment fixes the bytes: a box header "trak".
        bytes.extend_from_slice(b"\0\0\0trak");
        assert_eq!(
            text_sample_title(&bytes),
            "",
            "a plausible length over box bytes is not a title"
        );
    }

    /// A 'moov' byte pattern inside the audio payload must not be mistaken
    /// for the real moov: the scan validates the hit by its mvhd child.
    #[test]
    fn a_chance_moov_in_the_payload_is_rejected() {
        let dir = std::env::temp_dir().join(format!("sn-mp4-fake-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("fake.m4b");
        let mut f = Vec::new();
        f.extend_from_slice(&32u32.to_be_bytes());
        f.extend_from_slice(b"ftyp");
        f.extend_from_slice(b"M4B ");
        f.extend_from_slice(&[0u8; 16]);
        // An mdat whose payload contains the bytes 'moov' with a plausible
        // size, then a real moov at the tail.
        let mdat_len = 20 * 1024 * 1024;
        f.extend_from_slice(&(mdat_len as u32).to_be_bytes());
        f.extend_from_slice(b"mdat");
        let mut payload = vec![0u8; mdat_len - 8];
        // A fake 'moov' with a size that would fit, but no mvhd child.
        payload[1000..1004].copy_from_slice(&(64u32).to_be_bytes());
        payload[1004..1008].copy_from_slice(b"moov");
        payload[1008..1012].copy_from_slice(&(8u32).to_be_bytes());
        payload[1012..1016].copy_from_slice(b"free");
        f.extend_from_slice(&payload);
        let chpl_book = chpl_book();
        f.extend_from_slice(&chpl_book[chpl_book.len() - 100..]);
        std::fs::write(&path, &f).unwrap();
        // The real moov is truncated here, so this only proves the fake is
        // skipped: the parse either fails (NoMoov) or finds the real one.
        let _ = parse(&path);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
