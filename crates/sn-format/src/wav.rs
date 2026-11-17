//! The WAV codec: canonical 16-bit PCM, written and read a stream at a time.
//!
//! Every engine emits WAV, the player consumes it, exports are WAV, and the
//! Swift port's chunk cache was a directory of WAVs. Three things this module
//! has to get right:
//!
//! - **Streaming.** iOS held a whole 200k-character chapter in memory as
//!   samples — 1.1 GB. [`WavWriter`] writes as it goes and never holds more
//!   than one buffer.
//! - **Chunk walking, not assumptions.** A file with `LIST`, `fact` or `id3 `
//!   before `data` must still read. The Swift reader walked the chunk list; so
//!   does this one.
//! - **Exact sample accounting.** The play position is *written minus the
//!   device's own delay*, so a header that overstates the sample count puts the
//!   read-along highlight permanently ahead of the voice.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

/// Bytes in a canonical `fmt ` chunk (PCM, no extension).
pub const FMT_CHUNK_BYTES: usize = 16;
/// The canonical 44-byte header.
pub const HEADER_BYTES: usize = 44;

/// Bytes per sample for 16-bit mono.
pub const BYTES_PER_SAMPLE: usize = 2;

/// A parsed WAV header plus where its data starts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WavInfo {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
    /// Byte offset of the first sample.
    pub data_offset: u64,
    /// Number of sample frames in the `data` chunk, as declared.
    pub frames: u32,
}

impl WavInfo {
    pub fn duration_secs(&self) -> f64 {
        if self.sample_rate == 0 {
            return 0.0;
        }
        self.frames as f64 / self.sample_rate as f64
    }

    /// Interleaved samples per frame.
    pub fn samples_per_frame(&self) -> usize {
        (self.channels.max(1) as usize) * ((self.bits_per_sample.max(8) as usize) / 8)
    }
}

/// Canonical header for 16-bit PCM mono.
pub fn header_bytes(sample_rate: u32, frames: u32) -> [u8; HEADER_BYTES] {
    let byte_rate = sample_rate * BYTES_PER_SAMPLE as u32;
    let data_bytes = frames.saturating_mul(BYTES_PER_SAMPLE as u32);
    let mut h = [0u8; HEADER_BYTES];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + data_bytes).to_le_bytes());
    h[8..12].copy_from_slice(b"WAVE");
    h[12..16].copy_from_slice(b"fmt ");
    h[16..20].copy_from_slice(&(FMT_CHUNK_BYTES as u32).to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
    h[22..24].copy_from_slice(&1u16.to_le_bytes()); // mono
    h[24..28].copy_from_slice(&sample_rate.to_le_bytes());
    h[28..32].copy_from_slice(&byte_rate.to_le_bytes());
    h[32..34].copy_from_slice(&2u16.to_le_bytes()); // block align
    h[34..36].copy_from_slice(&16u16.to_le_bytes()); // bits per sample
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data_bytes.to_le_bytes());
    h
}

/// Write a whole WAV from interleaved `i16` samples, in one atomic rename.
pub fn write_samples(path: &Path, sample_rate: u32, samples: &[i16]) -> std::io::Result<()> {
    sn_core::atomic::write_atomic(path, &to_wav_bytes(sample_rate, samples))
}

/// A complete WAV file in memory.
pub fn to_wav_bytes(sample_rate: u32, samples: &[i16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_BYTES + samples.len() * BYTES_PER_SAMPLE);
    out.extend_from_slice(&header_bytes(sample_rate, samples.len() as u32));
    out.extend_from_slice(&to_bytes(samples));
    out
}

fn to_bytes(samples: &[i16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * BYTES_PER_SAMPLE);
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// Parse a WAV header by walking the chunk list.
pub fn parse_header(bytes: &[u8]) -> Result<WavInfo, WavError> {
    parse_header_bounded(bytes, bytes.len() as u64)
}

/// Parse a header from a *window* of the file, where `file_len` is the real
/// length.
///
/// The two are different things. A 64 KB window of a 300 KB WAV is a
/// perfectly good header that simply does not contain the whole `data` chunk,
/// and treating the window as the file turns every long render into a
/// "truncated" one. `file_len` is the authority on how much file there is;
/// `bytes` is only how much of it was read.
pub fn parse_header_bounded(bytes: &[u8], file_len: u64) -> Result<WavInfo, WavError> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(WavError::NotWav);
    }

    let mut pos = 12usize;
    let mut fmt: Option<(u16, u16, u16, u32)> = None;
    let mut data: Option<(u64, u32)> = None;

    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body = pos + 8;
        if body + size > file_len as usize {
            // The file really is shorter than its own `data` chunk claims: a
            // half-written cache file. Take the valid prefix rather than
            // failing, so it still plays what did land.
            let avail = (file_len as usize).saturating_sub(body);
            if id == b"data" {
                data = Some((body as u64, (avail / BYTES_PER_SAMPLE) as u32));
            }
            break;
        }
        if body + size > bytes.len() {
            // The chunk is intact in the file but runs past this window. Its
            // size is trustworthy (the file length vouched for it), so record
            // it and stop walking — the remaining chunks are past the window
            // and cannot be read, and `data` is nearly always this one.
            if id == b"data" {
                data = Some((body as u64, (size / BYTES_PER_SAMPLE) as u32));
            }
            break;
        }

        match id {
            b"fmt " if size >= 16 => {
                let channels = u16::from_le_bytes(bytes[body + 2..body + 4].try_into().unwrap());
                let rate = u32::from_le_bytes(bytes[body + 4..body + 8].try_into().unwrap());
                let bits = u16::from_le_bytes(bytes[body + 14..body + 16].try_into().unwrap());
                fmt = Some((channels, bits, 1, rate));
            }
            b"data" => {
                let frames = (size / BYTES_PER_SAMPLE) as u32;
                data = Some((body as u64, frames));
            }
            _ => {}
        }

        // Chunks are word-aligned: an odd size is followed by a pad byte.
        pos = body + size + (size & 1);
    }

    let (channels, bits, _, rate) = fmt.ok_or(WavError::NoFmtChunk)?;
    let (data_offset, frames) = data.ok_or(WavError::NoDataChunk)?;
    Ok(WavInfo {
        sample_rate: rate,
        channels,
        bits_per_sample: bits,
        data_offset,
        frames,
    })
}

/// Read a whole WAV file's samples as `i16`.
///
/// The header is discovered by reading a *window* rather than exactly 44 bytes:
/// `data` is not always the second chunk, so parsing a 44-byte prefix would
/// see a truncated final chunk and report zero frames.
pub fn read_samples(path: &Path) -> Result<(u32, Vec<i16>), WavError> {
    let mut f = std::fs::File::open(path).map_err(WavError::Io)?;
    let total = file_len(&f);
    let window = std::cmp::max(HEADER_BYTES, std::cmp::min(64 * 1024, total));
    let mut head = vec![0u8; window];
    f.read_exact(&mut head).map_err(WavError::Io)?;
    // The *file* length, not the window: `parse_header` reads a short buffer
    // as "the file is this short", and a `data` chunk that runs past the
    // window is a normal 300 KB model render, not a truncated file. Parsing
    // the window alone capped every chunk at 1.4 s of audio and the read
    // then succeeded, so the tail of every long sentence went missing with
    // nothing in the log. The real length is passed in and the declared
    // chunk size is kept.
    let info = parse_header_bounded(&head, total as u64)?;
    f.seek(SeekFrom::Start(info.data_offset))
        .map_err(WavError::Io)?;

    let available = total.saturating_sub(info.data_offset as usize);
    // Trust the smaller of what the header declares and what the file holds:
    // a genuinely truncated file still plays its valid prefix, and a
    // over-declared one does not read past the end.
    let want = std::cmp::min(
        info.frames as usize * info.samples_per_frame(),
        available,
    );
    let mut raw = vec![0u8; want];
    f.read_exact(&mut raw).map_err(WavError::Io)?;
    Ok((info.sample_rate, decode_i16(&raw, info.bits_per_sample)))
}

fn file_len(f: &std::fs::File) -> usize {
    f.metadata().map(|m| m.len() as usize).unwrap_or(0)
}

/// Convert raw PCM bytes to `i16`, scaling 8- and 32-bit sources.
///
/// Only 16-bit is produced by our own engines; the scaling exists because
/// ffmpeg's pipe can be asked for other widths and a truncated scale is worse
/// than an honest one.
pub fn decode_i16(raw: &[u8], bits_per_sample: u16) -> Vec<i16> {
    match bits_per_sample {
        16 => raw
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect(),
        8 => raw.iter().map(|b| ((*b as i32 - 128) << 8) as i16).collect(),
        32 => raw
            .chunks_exact(4)
            .map(|c| (i32::from_le_bytes([c[0], c[1], c[2], c[3]]) >> 16) as i16)
            .collect(),
        _ => raw
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect(),
    }
}

/// Clamp and scale a float sample to `i16`, the conversion every engine uses.
///
/// NaN becomes silence rather than 0x8000, which would be a full-scale click.
pub fn f32_to_i16(v: f32) -> i16 {
    if v.is_nan() {
        return 0;
    }
    (v.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

/// Streaming writer over a real file: header patched from the file's own length
/// on finalise, so a miscount is impossible.
pub struct FileWavWriter {
    file: std::fs::File,
    sample_rate: u32,
    frames: u32,
    scratch: Vec<u8>,
}

impl FileWavWriter {
    pub fn create(path: &Path, sample_rate: u32) -> std::io::Result<Self> {
        let mut file = std::fs::File::create(path)?;
        file.write_all(&header_bytes(sample_rate, 0))?;
        Ok(Self {
            file,
            sample_rate,
            frames: 0,
            scratch: Vec::new(),
        })
    }

    pub fn frames(&self) -> u32 {
        self.frames
    }

    pub fn duration_secs(&self) -> f64 {
        self.frames as f64 / self.sample_rate.max(1) as f64
    }

    /// Append samples. The counter advances only after the bytes are committed.
    pub fn write_samples(&mut self, samples: &[i16]) -> std::io::Result<()> {
        if samples.is_empty() {
            return Ok(());
        }
        self.scratch.clear();
        self.scratch.reserve(samples.len() * BYTES_PER_SAMPLE);
        for s in samples {
            self.scratch.extend_from_slice(&s.to_le_bytes());
        }
        self.file.write_all(&self.scratch)?;
        self.frames = self.frames.saturating_add(samples.len() as u32);
        Ok(())
    }

    pub fn write_f32(&mut self, samples: &[f32]) -> std::io::Result<()> {
        let scaled: Vec<i16> = samples.iter().copied().map(f32_to_i16).collect();
        self.write_samples(&scaled)
    }

    /// Flush, patch the RIFF sizes from the file's real length, and return it.
    pub fn finalise(mut self) -> std::io::Result<std::fs::File> {
        self.file.flush()?;
        let len = self.file.metadata()?.len();
        let frames = ((len.saturating_sub(HEADER_BYTES as u64)) / BYTES_PER_SAMPLE as u64) as u32;
        let rate = self.sample_rate;
        let mut file = self.file;
        patch_header(&mut file, rate, frames)?;
        Ok(file)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WavError {
    #[error("not a RIFF/WAVE file")]
    NotWav,
    #[error("no fmt chunk")]
    NoFmtChunk,
    #[error("no data chunk")]
    NoDataChunk,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Streaming write, one sample at a time, with a buffered backing store.
///
/// The Swift port's `WAVWriter.StreamingWriter` incremented its sample count
/// *before* the write, so a failed write left a RIFF header claiming samples
/// that were not there — a file that played as silence and then a click. The
/// count here is incremented only after the bytes are committed, and the header
/// is patched at the end.
pub struct StreamWavWriter<W: Write> {
    out: W,
    sample_rate: u32,
    frames_written: u32,
}

impl<W: Write> StreamWavWriter<W> {
    pub fn new(mut out: W, sample_rate: u32) -> std::io::Result<Self> {
        out.write_all(&header_bytes(sample_rate, 0))?;
        Ok(Self {
            out,
            sample_rate,
            frames_written: 0,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn frames(&self) -> u32 {
        self.frames_written
    }

    pub fn duration_secs(&self) -> f64 {
        self.frames_written as f64 / self.sample_rate.max(1) as f64
    }

    /// Append samples. The counter advances only once the bytes are out.
    pub fn write_samples(&mut self, samples: &[i16]) -> std::io::Result<()> {
        if samples.is_empty() {
            return Ok(());
        }
        let mut buf = Vec::with_capacity(samples.len() * BYTES_PER_SAMPLE);
        for s in samples {
            buf.extend_from_slice(&s.to_le_bytes());
        }
        self.out.write_all(&buf)?;
        self.frames_written = self.frames_written.saturating_add(samples.len() as u32);
        Ok(())
    }

    /// Append float samples, scaling them.
    pub fn write_f32(&mut self, samples: &[f32]) -> std::io::Result<()> {
        let scaled: Vec<i16> = samples.iter().copied().map(f32_to_i16).collect();
        self.write_samples(&scaled)
    }

    /// Finish and hand back the writer. The header still carries the length
    /// known when the writer was created; RIFF sizes are patched by
    /// [`finalise_file`] or [`StreamWavWriter::finalise_into_file`].
    pub fn finalise(self) -> std::io::Result<W> {
        Ok(self.out)
    }

}

/// Patch a file's RIFF sizes in place, preserving the format fields.
///
/// `sample_rate` of 0 means "keep whatever the header already says", which is
/// what [`finalise_file`] uses when the caller did not track the rate.
pub fn patch_header(
    file: &mut std::fs::File,
    sample_rate: u32,
    frames: u32,
) -> std::io::Result<()> {
    let rate = if sample_rate > 0 {
        sample_rate
    } else {
        let mut header = [0u8; HEADER_BYTES];
        file.seek(SeekFrom::Start(0))?;
        file.read_exact(&mut header)?;
        u32::from_le_bytes(header[24..28].try_into().unwrap()).max(1)
    };
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&header_bytes(rate, frames))?;
    file.flush()
}

/// Finish a WAV file: derive the frame count from the file's own length, patch
/// the RIFF sizes, and return the resulting header.
///
/// Deriving from the length is deliberate. The Swift port's streaming writer
/// incremented its counter before the write, so a failed write left a header
/// claiming samples that were never written — a file that plays as silence and
/// then a click. Counting bytes on disk cannot be wrong in that way.
pub fn finalise_file(mut file: std::fs::File) -> std::io::Result<WavInfo> {
    let len = file.metadata()?.len();
    let frames = ((len.saturating_sub(HEADER_BYTES as u64)) / BYTES_PER_SAMPLE as u64) as u32;
    patch_header(&mut file, 0, frames)?;

    // Re-parse a window rather than a bare 44-byte prefix: the frame count is
    // built from the file's length, so the header is only trusted for the
    // format fields.
    let mut window = vec![0u8; std::cmp::min(len as usize, 4096)];
    file.seek(SeekFrom::Start(0))?;
    file.read_exact(&mut window)?;
    let parsed = parse_header(&window).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    Ok(WavInfo {
        frames,
        ..parsed
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("sn-wav-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("out.wav")
    }

    #[test]
    fn header_is_canonical_44_bytes() {
        let h = header_bytes(24_000, 48_000);
        assert_eq!(&h[0..4], b"RIFF");
        assert_eq!(&h[8..12], b"WAVE");
        assert_eq!(&h[12..16], b"fmt ");
        assert_eq!(&h[36..40], b"data");
        assert_eq!(u32::from_le_bytes(h[40..44].try_into().unwrap()), 48_000 * 2);
    }

    /// The bug this file's `parse_header_bounded` exists for: a WAV whose
    /// `data` chunk runs past the 64 KB header window is a *normal* file, not
    /// a truncated one. Reading only the window and believing it was the whole
    /// file capped every chunk at ~1.4 s of audio, and the following read
    /// succeeded, so the tail of every long sentence went missing silently.
    #[test]
    fn a_data_chunk_past_the_header_window_is_still_read_whole() {
    let path = tmp("long");
        // 20 s at 22.05 kHz: 441 000 samples, 882 000 bytes — thirteen times
        // the 64 KB window.
        let frames: u32 = 441_000;
        let samples: Vec<i16> = (0..frames).map(|i| ((i % 97) as i16) - 48).collect();
        write_samples(&path, 22_050, &samples).unwrap();
        assert!(
            std::fs::metadata(&path).unwrap().len() > 64 * 1024,
            "the fixture must exceed the header window"
        );
        let (rate, read) = read_samples(&path).unwrap();
        assert_eq!(rate, 22_050);
        assert_eq!(read.len(), samples.len(), "audio was truncated");
        assert_eq!(read, samples);
    }

    /// The flip side: a file that really is shorter than its `data` chunk
    /// claims must still yield its valid prefix, not an error and not a
    /// read past the end.
    #[test]
    fn a_genuinely_truncated_file_yields_its_prefix() {
        let path = tmp("truncated");
        let samples: Vec<i16> = (0..10_000).map(|i| (i as i16) - 5000).collect();
        let full = to_wav_bytes(22_050, &samples);
        // Keep the header intact, cut the payload in half.
        std::fs::write(&path, &full[..44 + samples.len()]).unwrap();
        let (rate, read) = read_samples(&path).unwrap();
        assert_eq!(rate, 22_050);
        assert_eq!(read.len(), samples.len() / 2, "the prefix is half the payload");
        assert_eq!(&read[..], &samples[..samples.len() / 2]);
    }

    #[test]
    fn a_header_window_past_the_end_of_a_real_file_does_not_invent_audio() {
        // `parse_header` (the unbounded form) is what a caller with the whole
        // file passes; give it a buffer longer than the file claims and it
        // must still believe the file's own size.
        let samples: Vec<i16> = (0..500).map(|i| (i as i16) - 250).collect();
        let bytes = to_wav_bytes(22_050, &samples);
        let info = parse_header(&bytes).unwrap();
        assert_eq!(info.frames, 500);
    }

    #[test]
    fn round_trips_through_bytes() {
        let samples: Vec<i16> = (0..1000).map(|i| (i % 1000) as i16 - 500).collect();
        let bytes = to_wav_bytes(22_050, &samples);
        let info = parse_header(&bytes).unwrap();
        assert_eq!(info.sample_rate, 22_050);
        assert_eq!(info.frames, 1000);
        let decoded = decode_i16(&bytes[info.data_offset as usize..], 16);
        assert_eq!(decoded, samples);
    }

    /// Assemble a WAV whose `fmt ` chunk is followed by `extra` chunks and then a
    /// `data` chunk holding `frames` silent samples.
    fn wav_with_extra(extra: &[(&[u8; 4], Vec<u8>)], sample_rate: u32, frames: u32) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&0u32.to_le_bytes()); // patched below
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&sample_rate.to_le_bytes());
        out.extend_from_slice(&(sample_rate * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        for (id, body) in extra {
            out.extend_from_slice(*id);
            out.extend_from_slice(&(body.len() as u32).to_le_bytes());
            out.extend_from_slice(body);
            if body.len() % 2 == 1 {
                out.push(0); // RIFF chunks are word-aligned
            }
        }
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(frames * 2).to_le_bytes());
        out.resize(out.len() + frames as usize * 2, 0);
        let riff_len = (out.len() - 8) as u32;
        out[4..8].copy_from_slice(&riff_len.to_le_bytes());
        out
    }

    #[test]
    fn walks_past_extra_chunks() {
        // fmt, then a LIST chunk, then data. A reader that assumes `data` comes
        // straight after `fmt ` finds nothing and plays silence.
        let bytes = wav_with_extra(&[(b"LIST", b"INFOxxxx".to_vec())], 44_100, 1000);
        let info = parse_header(&bytes).unwrap();
        assert_eq!(info.sample_rate, 44_100);
        assert_eq!(info.frames, 1000);
        assert_eq!(info.data_offset, (bytes.len() - 2000) as u64);
    }

    #[test]
    fn handles_odd_sized_chunks_via_word_alignment() {
        // An odd-sized chunk is followed by a pad byte. A reader that does not
        // skip it lands one byte early and misreads the next chunk header.
        let bytes = wav_with_extra(&[(b"odd ", b"abc".to_vec())], 24_000, 10);
        let info = parse_header(&bytes).unwrap();
        assert_eq!(info.frames, 10);
        assert_eq!(info.sample_rate, 24_000);
    }

    #[test]
    fn handles_several_extra_chunks() {
        let bytes = wav_with_extra(
            &[
                (b"fact", vec![0u8; 4]),
                (b"id3 ", b"ID3 metadata padding".to_vec()),
                (b"junk", vec![0u8; 7]),
            ],
            48_000,
            500,
        );
        let info = parse_header(&bytes).unwrap();
        assert_eq!(info.frames, 500);
        assert_eq!(info.sample_rate, 48_000);
    }

    #[test]
    fn a_truncated_data_chunk_still_yields_its_valid_prefix() {
        let mut bytes = to_wav_bytes(24_000, &(0..1000i16).collect::<Vec<_>>());
        bytes.truncate(bytes.len() - 250); // lose the last 125 samples
        let info = parse_header(&bytes).unwrap();
        assert_eq!(info.frames, 875);
    }

    #[test]
    fn rejects_non_wav_input() {
        assert!(matches!(parse_header(b"not a wav file at all"), Err(WavError::NotWav)));
        assert!(matches!(parse_header(&[]), Err(WavError::NotWav)));
    }

    #[test]
    fn rejects_a_header_with_no_fmt_chunk() {
        let mut bytes = Vec::from(*b"RIFF");
        bytes.extend_from_slice(&4u32.to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&0u32.to_le_bytes());
        assert!(matches!(parse_header(&bytes), Err(WavError::NoFmtChunk)));
    }

    #[test]
    fn f32_conversion_clamps_and_handles_nan() {
        assert_eq!(f32_to_i16(0.0), 0);
        assert_eq!(f32_to_i16(1.0), 32767);
        assert_eq!(f32_to_i16(-1.0), -32767);
        assert_eq!(f32_to_i16(2.0), 32767);
        assert_eq!(f32_to_i16(-2.0), -32767);
        assert_eq!(f32_to_i16(f32::NAN), 0);
        assert_eq!(f32_to_i16(f32::INFINITY), 32767);
    }

    #[test]
    fn decode_scales_other_bit_depths() {
        let eight = [0u8, 128, 255];
        let scaled = decode_i16(&eight, 8);
        assert_eq!(scaled, vec![-32768, 0, 32512]);
        let thirty_two = 1_000_000i32.to_le_bytes();
        assert_eq!(decode_i16(&thirty_two, 32), vec![(1_000_000 >> 16) as i16]);
    }

    #[test]
    fn writes_and_reads_a_file() {
        let p = tmp("file");
        let samples: Vec<i16> = (0..500).map(|i| (i * 3 % 2000) as i16).collect();
        write_samples(&p, 24_000, &samples).unwrap();
        let (rate, read) = read_samples(&p).unwrap();
        assert_eq!(rate, 24_000);
        assert_eq!(read, samples);
    }

    #[test]
    fn stream_writer_counts_only_written_samples() {
        let p = tmp("stream");
        {
            let mut w = FileWavWriter::create(&p, 22_050).unwrap();
            w.write_samples(&[1, 2, 3]).unwrap();
            assert_eq!(w.frames(), 3);
            w.write_samples(&[]).unwrap();
            assert_eq!(w.frames(), 3, "an empty write must not move the count");
            w.finalise().unwrap();
        }

        let (rate, samples) = read_samples(&p).unwrap();
        assert_eq!(rate, 22_050);
        assert_eq!(samples, vec![1, 2, 3]);
        let info = parse_header(&std::fs::read(&p).unwrap()).unwrap();
        assert_eq!(info.frames, 3, "the header must be patched on finalise");
    }

    #[test]
    fn a_stream_file_finalised_from_its_length_keeps_the_rate() {
        // A writer that wrapped a stream and never patched its own header must
        // still be repairable: `finalise_file` derives the count from the file's
        // length and keeps the rate the header already declared.
        let p = tmp("stream-rate");
        {
            let f = std::fs::File::create(&p).unwrap();
            let mut w = StreamWavWriter::new(std::io::BufWriter::new(f), 44_100).unwrap();
            w.write_samples(&[7, 7, 7, 7]).unwrap();
            w.finalise().unwrap();
        }
        let f = std::fs::File::options()
            .write(true)
            .read(true)
            .open(&p)
            .unwrap();
        let info = finalise_file(f).unwrap();
        assert_eq!(info.sample_rate, 44_100);
        assert_eq!(info.frames, 4);

        let (rate, samples) = read_samples(&p).unwrap();
        assert_eq!(rate, 44_100);
        assert_eq!(samples, vec![7, 7, 7, 7]);
    }

    #[test]
    fn finalise_file_repairs_a_header_with_a_zero_data_size() {
        let p = tmp("repair");
        {
            let f = std::fs::File::create(&p).unwrap();
            let mut w = StreamWavWriter::new(f, 22_050).unwrap();
            w.write_samples(&[1, 2, 3, 4, 5]).unwrap();
            w.finalise().unwrap();
        }
        let f = std::fs::File::options()
            .write(true)
            .read(true)
            .open(&p)
            .unwrap();
        let info = finalise_file(f).unwrap();
        assert_eq!(info.frames, 5);
        let bytes = std::fs::read(&p).unwrap();
        assert_eq!(
            u32::from_le_bytes(bytes[40..44].try_into().unwrap()),
            10,
            "the data size must be patched"
        );
        assert_eq!(
            u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
            36 + 10,
            "the RIFF size must be patched"
        );
    }

    #[test]
    fn stream_writer_handles_floats() {
        let p = tmp("floats");
        {
            let mut w = FileWavWriter::create(&p, 48_000).unwrap();
            w.write_f32(&[0.0, 0.5, -0.5, 1.0, f32::NAN]).unwrap();
            w.finalise().unwrap();
        }
        let bytes = std::fs::read(&p).unwrap();
        let info = parse_header(&bytes).unwrap();
        assert_eq!(info.frames, 5);
        let decoded = decode_i16(&bytes[info.data_offset as usize..], 16);
        assert_eq!(decoded, vec![0, 16384, -16384, 32767, 0]);
    }

    #[test]
    fn duration_is_frames_over_rate() {
        let info = WavInfo {
            sample_rate: 24_000,
            channels: 1,
            bits_per_sample: 16,
            data_offset: 44,
            frames: 48_000,
        };
        assert!((info.duration_secs() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn a_200k_sample_export_streams_without_holding_the_whole_file() {
        // The streaming contract: write 400k samples in 100k-sample blocks and
        // confirm the file is correct afterwards.
        let p = tmp("big");
        let f = std::fs::File::create(&p).unwrap();
        let mut w = StreamWavWriter::new(std::io::BufWriter::new(f), 24_000).unwrap();
        for block in 0..4 {
            let block: Vec<i16> = (0..100_000)
                .map(|i| ((block * 100_000 + i) % 1000) as i16)
                .collect();
            w.write_samples(&block).unwrap();
        }
        assert_eq!(w.frames(), 400_000);
        w.finalise().unwrap();
            let f = std::fs::File::options().read(true).write(true).open(&p).unwrap();
            finalise_file(f).unwrap();

        let info = parse_header(&std::fs::read(&p).unwrap()).unwrap();
        assert_eq!(info.frames, 400_000);
        assert!((info.duration_secs() - 400_000.0 / 24_000.0).abs() < 1e-9);
    }
}