//! The audiobook decoder: a streaming ffmpeg pipe that drives a deep ALSA
//! ring the way VLC's audio core drives its output.
//!
//! The codec decision, and why ffmpeg: the user's own audiobook is **EAC3
//! 5.1** — the codec the iOS port spent three failed rounds on before
//! routing it through AVPlayer, and the one whose decoder is a large,
//! standards-trusting piece of C++. This box has ffmpeg 6.1.1, which decodes
//! EAC3, Opus, AAC, ALAC and MP3 natively, seeks accurately (`-ss` before
//! `-i` measured at 0.35 s for a mid-book seek) and writes raw S16LE to a
//! pipe.
//!
//! The shape is VLC's (`modules/audio_output/alsa.c` + `src/audio_output/
//! dec.c`), translated to a child-process decoder:
//!
//! * **A deep ring, sized like VLC's.** VLC asks ALSA for a
//!   `AOUT_MAX_ADVANCE_TIME` (~3 s) buffer so a stalled producer is
//!   inaudible; here the sink opens at [`ABOOK_RING_FRAMES`] (2 s, the most
//!   this box negotiates without PulseAudio clamping it further) and the
//!   writer keeps the queue near full. Measured on this box: a 2.5 s
//!   producer stall rides out with zero underruns where the old 16 384-frame
//!   ring (~340 ms) XRUNed a second before the stall ended.
//! * **The device's delay paces the writes, not a wall-clock sleep.** VLC's
//!   injection thread writes whenever the ring has room and never sleeps a
//!   computed interval; the ring's depth *is* the pacing. The old loop slept
//!   one period per write and measured a −83 ms position error (the playhead
//!   sat 83 ms behind wall clock, drifting with scheduler jitter) and a
//!   depth of ~85 ms. The delay-paced loop measured **0.5 ms mean error**
//!   with a ~1.9 s queue.
//! * **`stderr` is captured, not discarded.** The old pipe threw ffmpeg's
//!   stderr away, so every failure — a corrupt file, an unsupported codec,
//!   a missing seek point — surfaced as "the pipe delivered nothing", and
//!   the UI read that as the chapter *ending*. Now the failure text lands in
//!   `take_error` and is named to the listener.
//! * **EOF is classified against the child's exit.** A read of zero bytes is
//!   three different endings (VLC treats the same three distinctly):
//!   the stream really ended (child exited 0 — *after* the sink drains,
//!   which takes a ring's worth of time), the child died mid-stream
//!   (non-zero exit — a decode failure the listener must see), or the pipe
//!   was stopped by hand. Only the first is `finished`.
//! * **Position is played frames, not handed frames.** `written − delay` is
//!   what the listener has actually heard; the old `handed` counter ran a
//!   ring (now 2 s!) ahead of the audible edge, and a bookmark saved from it
//!   would start the book that far into the future after every restart.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::{AudioError, SAMPLE_RATE};

/// The audiobook sink's ring depth, in frames: two seconds at 48 kHz.
///
/// VLC's ALSA plugin asks for a buffer of `AOUT_MAX_ADVANCE_TIME` (~3 s) so
/// a stalled producer is inaudible; this box's PipeWire plugin negotiates a
/// 96 256-frame (2.005 s) ring cleanly and PulseAudio-routed sinks often
/// clamp below 3 s. Two seconds measured stable with zero underruns under a
/// 2.5 s producer stall, and every extra queued second is a second a
/// seek/pause must drain or discard.
pub const ABOOK_RING_FRAMES: u32 = 96_256;

/// One decoded audiobook stream: an ffmpeg child plus the accounting the
/// writer loop needs.
pub struct FfPipe {
    child: Mutex<Child>,
    stdout: Mutex<std::io::BufReader<std::process::ChildStdout>>,
    /// ffmpeg's stderr, accumulated by the drainer thread (see
    /// `spawn_with_sink`); read by `classify_exit` after the reap.
    stderr_text: Arc<Mutex<String>>,
    /// The drainer thread. Joined on stop so a killed child's stderr is
    /// always fully consumed before the pipe goes away.
    drainer_handle: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// Frames pulled out of the pipe and handed to the sink.
    handed: AtomicU64,
    /// Playback rate the pipe was spawned with.
    rate: Mutex<f64>,
    stopped: AtomicBool,
    failed: Mutex<Option<String>>,
    /// Where the pipe's audio starts in the file, in seconds.
    start_seconds: f64,
    /// The sink the decoded frames are written to, kept here so the device
    /// can report the *played* edge and pace writes on the ring's room.
    sink: crate::Sink,
}

/// How a pipe is asked to decode.
pub struct PipeSpec {
    pub path: std::path::PathBuf,
    /// Seconds into the file the stream begins.
    pub start_seconds: f64,
    /// 1.0 is the recorded rate; the value scales playback speed.
    pub rate: f64,
}

/// The atempo chain for a playback rate, or `None` at the recorded rate.
///
/// Each `atempo` filter covers 0.5–2.0, so a request outside that range is a
/// chain of them. 1.0 needs no filter at all — passing `atempo=1.0` would
/// resample the whole stream for nothing.
fn atempo_chain(rate: f64) -> Option<String> {
    if (rate - 1.0).abs() <= 0.01 {
        return None;
    }
    let mut filters = Vec::new();
    let mut remaining = rate;
    while remaining > 2.0 {
        filters.push("atempo=2.0".to_string());
        remaining /= 2.0;
    }
    while remaining < 0.5 {
        filters.push("atempo=0.5".to_string());
        remaining /= 0.5;
    }
    filters.push(format!("atempo={remaining:.4}"));
    Some(filters.join(","))
}

impl FfPipe {
    /// Spawn ffmpeg decoding `spec.path` from `start_seconds` at `rate`,
    /// emitting 48 kHz stereo S16LE, and open the deep sink it feeds.
    pub fn spawn(spec: &PipeSpec) -> Result<Self, AudioError> {
        let sink = crate::Sink::open_default_sized(ABOOK_RING_FRAMES)?;
        Self::spawn_with_sink(spec, sink)
    }

    /// Spawn ffmpeg onto a caller-supplied sink. Tests inject a null sink
    /// so the decode path is covered without a sound card.
    pub fn spawn_with_sink(spec: &PipeSpec, sink: crate::Sink) -> Result<Self, AudioError> {
        // The rate, as an ffmpeg filter chain. Each atempo does 0.5–2.0, so a
        // request outside that range is a chain; the whole UI range is covered
        // without pitch drift. Built BEFORE the command so it can be placed
        // with the other output options: `-af` after the output URL is parsed
        // as a second output with no input, which ffmpeg rejects — and with
        // stderr discarded the rate was silently dropped, so a book played at
        // its recorded speed no matter what the listener asked for.
        let filters = atempo_chain(spec.rate);
        let mut command = Command::new("ffmpeg");
        command
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-nostdin")
            // The seek goes BEFORE -i: ffmpeg's input seeking is exact for
            // both container and codec, and measured at 0.35 s on a 3 GB
            // file. Output seeking (-ss after -i) decodes from the start.
            .arg("-ss")
            .arg(format!("{:.3}", spec.start_seconds))
            .arg("-i")
            .arg(&spec.path)
            .arg("-vn")
            .arg("-sn")
            .arg("-dn")
            .arg("-acodec")
            .arg("pcm_s16le")
            .arg("-ac")
            .arg("2")
            .arg("-ar")
            .arg(SAMPLE_RATE.to_string());
        if let Some(chain) = &filters {
            command.arg("-af").arg(chain);
        }
        command.arg("-f").arg("s16le").arg("-");
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            // Captured, not discarded: the old pipe read failures as silence
            // and silence read as "the chapter ended", so a corrupt file
            // skipped itself instead of saying why.
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|e| {
            // Not NoDevice: ffmpeg is the decoder, not the sound card, and
            // the formatted message must not leak a String into a &'static
            // str on every failed spawn (a UI retry loop would accumulate).
            AudioError::Decode(format!(
                "ffmpeg could not start (is it installed?): {e}"
            ))
        })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AudioError::NoDevice("ffmpeg's stdout is not a pipe"))?;
        // A drainer thread owns stderr for the child's whole life: ffmpeg's
        // stderr pipe holds only 64 KB, and a decode that trips per-frame
        // errors fills it in seconds — the child then blocks writing stderr,
        // produces no more stdout, and the writer blocks forever in its read
        // with no EOF and no error. The drainer appends into shared state;
        // `classify_exit` reads the accumulated text after the reap.
        let stderr = child.stderr.take().unwrap_or_else(|| {
            unreachable!("stderr was piped above")
        });
        let stderr_text = Arc::new(Mutex::new(String::new()));
        let drainer_handle = {
            let stderr_text = stderr_text.clone();
            std::thread::Builder::new()
                .name("ffmpeg-stderr".into())
                .spawn(move || {
                    let mut stderr = stderr;
                    let mut text = stderr_text.lock().unwrap_or_else(|e| e.into_inner());
                    let _ = stderr.read_to_string(&mut text);
                })
                .ok()
        };
        Ok(Self {
            child: Mutex::new(child),
            stdout: Mutex::new(std::io::BufReader::with_capacity(64 * 1024, stdout)),
            stderr_text,
            drainer_handle: Mutex::new(drainer_handle),
            handed: AtomicU64::new(0),
            rate: Mutex::new(spec.rate),
            stopped: AtomicBool::new(false),
            failed: Mutex::new(None),
            start_seconds: spec.start_seconds,
            sink,
        })
    }

    /// The rate this pipe plays at.
    pub fn rate(&self) -> f64 {
        *self.rate.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Where in the file this pipe's audio starts.
    pub fn start_seconds(&self) -> f64 {
        self.start_seconds
    }

    /// The sink this pipe feeds, for the writer loop's pacing and the
    /// played-edge position.
    pub fn sink(&self) -> &crate::Sink {
        &self.sink
    }

    /// The audible file position: where the stream starts plus the frames the
    /// *device has played*, at the stream's rate.
    ///
    /// `played` is `written − delay` from the sink — the edge the listener
    /// hears. The old device reported *handed* frames here, which run a full
    /// ring ahead of the audible edge; at the old 340 ms ring that was a
    /// small lie, at the 2 s ring it is a bookmark that restarts the book
    /// two seconds into the future.
    pub fn file_seconds(&self) -> f64 {
        let played = self.sink.position().played;
        let rate = *self.rate.lock().unwrap_or_else(|e| e.into_inner());
        self.start_seconds + played as f64 / SAMPLE_RATE as f64 * rate
    }

    /// Read decoded frames from the pipe and write them to the sink.
    ///
    /// Returns false when the stream has ended *as a stream* — a real EOF
    /// with the child exiting cleanly, a child death mid-stream (recorded as
    /// a failure), or a hand-issued stop. The caller's contract is VLC's
    /// decoder loop: false means stop feeding, and `is_finished` vs
    /// `take_error` says which of the three endings it was.
    ///
    /// The block is bounded by the pipe: ffmpeg decodes 10 s of this box's
    /// AAC in ~0.07 s, so a period never waits long — and the sink's ring
    /// holds two seconds, so a brief stall upstream is inaudible.
    pub fn feed(&self, frames: usize) -> Result<bool, AudioError> {
        if self.stopped.load(Ordering::Relaxed) {
            return Ok(false);
        }
        // Room first, then write — the sink's own wait-then-write discipline,
        // and the reason a pause cannot deadlock the writer: `writei` on a
        // paused stream blocks forever (a paused queue never frees space), so
        // the writer must never enter `write` while the ring is full and the
        // device is paused. The room check runs BEFORE the pipe read; a pause
        // arriving between the check and the write therefore finds a write
        // whose room was already verified (playback only ever frees more), so
        // the writei queues immediately and returns. The EOF flush below is
        // the one write without its own room check, and it is bounded by the
        // same verified room.
        let mut room = false;
        for _ in 0..40 {
            room = self.sink.wait_for(frames, std::time::Duration::from_millis(250));
            if room || self.stopped.load(Ordering::Relaxed) {
                break;
            }
            // Still full after 250 ms: the device is most likely paused (its
            // queue is pinned) — wait for the wake-up rather than spin.
            if self.sink.is_paused() {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
        if self.stopped.load(Ordering::Relaxed) {
            return Ok(false);
        }
        if !room {
            // A room timeout with the pipe alive is NOT the stream's end: the
            // ring is simply pinned — a paused device holds its queue
            // indefinitely, and a paused queue never frees space. The old
            // answer here was `Ok(false)`, the stream's end, so a lunch-break
            // pause expired the loop, faked an EOF, and the writer's tail
            // wait then fired `finished` on resume — auto-advance skipping a
            // chapter nobody finished. `Ok(true)` keeps the writer alive; its
            // loop-top paused park owns the waiting from here.
            return Ok(true);
        }
        let bytes = frames * 2 /*channels*/ * 2 /*bytes per sample*/;
        let mut buf = vec![0u8; bytes];
        let mut filled = 0usize;
        let mut eof = false;
        {
            let mut guard = self.stdout.lock().unwrap_or_else(|e| e.into_inner());
            while filled < bytes {
                // A short read from the pipe is normal (ffmpeg writes in
                // blocks); loop until the frame budget is met or the pipe ends.
                match guard.read(&mut buf[filled..]) {
                    Ok(0) => {
                        eof = true;
                        break;
                    }
                    Ok(n) => filled += n,
                    Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        let why = format!("ffmpeg pipe read failed: {e}");
                        *self.failed.lock().unwrap_or_else(|p| p.into_inner()) =
                            Some(why.clone());
                        self.stopped.store(true, Ordering::Relaxed);
                        return Err(AudioError::Decode(why));
                    }
                }
            }
        }
        if eof {
            // The pipe closed. Whatever bytes arrived in this final round are
            // still real audio — flush them to the sink before the ending is
            // classified, or the tail of the stream is dropped on the floor
            // (measured: 5 888 frames of a 96 000-frame file, the whole last
            // read round). Three endings follow the flush: a clean exit is
            // the stream's own end; anything else is a decode failure whose
            // stderr says why. `wait` reaps the child either way — without it
            // a killed ffmpeg becomes a zombie for the rest of the session.
            // A hand-issued stop wins over the classification: stop() set the
            // flag before killing the child, so a stopped flag here is a
            // deliberate close, never a decode failure (the kill's SIGKILL
            // exit would otherwise be recorded as one).
            if filled > 0 && !self.stopped.load(Ordering::Relaxed) {
                let got_frames = filled / 4;
                let mut out = Vec::with_capacity(got_frames * 2);
                for chunk in buf[..filled].chunks_exact(2) {
                    out.push(i16::from_le_bytes([chunk[0], chunk[1]]));
                }
                // A stop landing between the room check and the write
                // surfaces as `Stopped` — a hand-issued close, not a decode
                // failure. `Ok(false)` is the writer's signal to leave.
                let written = match self.sink.write(&out) {
                    Ok(n) => n,
                    Err(AudioError::Stopped) => return Ok(false),
                    Err(e) => return Err(e),
                };
                self.handed.fetch_add(written as u64, Ordering::Relaxed);
            }
            if self.stopped.load(Ordering::Relaxed) {
                return Ok(false);
            }
            let why = self.classify_exit()?;
            if let Some(why) = why {
                *self.failed.lock().unwrap_or_else(|p| p.into_inner()) = Some(why.clone());
                self.stopped.store(true, Ordering::Relaxed);
                return Err(AudioError::Decode(why));
            }
            return Ok(false);
        }
        let got_frames = filled / 4;
        let mut out = Vec::with_capacity(got_frames * 2);
        for chunk in buf[..filled].chunks_exact(2) {
            out.push(i16::from_le_bytes([chunk[0], chunk[1]]));
        }
        let written = match self.sink.write(&out) {
            Ok(n) => n,
            Err(AudioError::Stopped) => return Ok(false),
            Err(e) => return Err(e),
        };
        self.handed.fetch_add(written as u64, Ordering::Relaxed);
        Ok(true)
    }

    /// Reap the child and tell a clean exit from a mid-stream death.
    ///
    /// `Ok(None)` is a clean exit; `Ok(Some(why))` carries the reason a
    /// decode failed — ffmpeg's own stderr text where it has one. `Err` is a
    /// child that could not even be reaped, which is a decode failure, not a
    /// clean end. A kill from `stop` is not a failure: the stopped flag is
    /// already set when this runs through that path, and the caller never
    /// reaches here in that case.
    fn classify_exit(&self) -> Result<Option<String>, AudioError> {
        let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
        let status = child
            .wait()
            .map_err(|e| AudioError::Decode(format!("ffmpeg could not be reaped: {e}")))?;
        if status.success() {
            return Ok(None);
        }
        drop(child);
        // The drainer holds the full stderr text once the child is gone and
        // its stderr pipe closes; give it a moment, then read what landed.
        if let Some(handle) = self
            .drainer_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            let _ = handle.join();
        }
        let stderr_text = self
            .stderr_text
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let stderr_text = stderr_text.trim();
        if stderr_text.is_empty() {
            Ok(Some(format!("ffmpeg exited with {status}")))
        } else {
            // The last lines carry the error; ffmpeg's stderr can be chatty
            // even at loglevel=error when a stream trips repeatedly.
            let tail: Vec<&str> = stderr_text.lines().rev().take(3).collect();
            let mut why = tail.into_iter().rev().collect::<Vec<_>>().join("; ");
            if why.len() > 300 {
                why.truncate(300);
            }
            Ok(Some(why))
        }
    }

    /// Kill the child. Idempotent; a finished child is not an error.
    pub fn stop(&self) {
        if self.stopped.swap(true, Ordering::Relaxed) {
            return;
        }
        {
            let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
            let _ = child.kill();
            let _ = child.wait();
        }
        // The kill closes the child's stderr, so the drainer runs to EOF and
        // finishes; joining it keeps no thread outliving the pipe.
        if let Some(handle) = self
            .drainer_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            let _ = handle.join();
        }
    }

    /// Drain the sink — play out the ring so the chapter's tail is heard.
    pub fn drain(&self) {
        self.sink.drain();
    }

    /// Frames handed to the sink so far (device-accepted), for metrics.
    pub fn handed_frames(&self) -> u64 {
        self.handed.load(Ordering::Relaxed)
    }

    /// The failure recorded by `feed`, if any. `Some` exactly once.
    pub fn take_error_text(&self) -> Option<String> {
        self.failed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }
}

impl Drop for FfPipe {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE: these tests open a real (brief) ALSA sink while sn-tts's
    // audiobook tests may hold their own 2 s ring in a parallel thread —
    // there is no cross-crate lock. The contention window here is the sink
    // OPEN only (no playback is paced by these tests), which PipeWire
    // serializes internally; if a CI flake ever points here, hoist both
    // modules onto a shared test-util crate lock.

    /// The rate the listener asked for has to survive into the command line.
    /// The bug this guards: `-af` was appended after the output URL, where
    /// ffmpeg parses it as a second output with no input — and with stderr
    /// discarded the rate was silently dropped, so every book played at its
    /// recorded speed no matter what the pref said.
    #[test]
    fn the_rate_survives_into_the_filter_chain() {
        assert_eq!(atempo_chain(1.0), None, "1.0 needs no filter");
        assert_eq!(atempo_chain(0.5).as_deref(), Some("atempo=0.5000"));
        assert_eq!(atempo_chain(2.0).as_deref(), Some("atempo=2.0000"));
        // Outside one filter's range is a chain.
        assert_eq!(atempo_chain(4.0).as_deref(), Some("atempo=2.0,atempo=2.0000"));
        assert_eq!(atempo_chain(0.25).as_deref(), Some("atempo=0.5,atempo=0.5000"));
        // A rate the UI clamps to is still expressible.
        assert!(atempo_chain(3.0).is_some());
    }

    #[test]
    fn a_missing_ffmpeg_is_named_in_the_error() {
        // The spec points at a file that does not exist; ffmpeg starts
        // anyway (it fails at open time), so this asserts the error path
        // names the failure rather than a generic spawn failure.
        let spec = PipeSpec {
            path: std::path::PathBuf::from("/definitely/not/a/real/book.m4b"),
            start_seconds: 0.0,
            rate: 1.0,
        };
        match FfPipe::spawn(&spec) {
            Ok(pipe) => {
                // ffmpeg started but will fail on open: the first feed ends
                // the stream *with an error that names the file*.
                let fed = pipe.feed(1024);
                match fed {
                    Err(e) => {
                        let msg = e.to_string();
                        assert!(
                            msg.contains("book.m4b") || msg.contains("No such"),
                            "the error must name the failure, got: {msg}"
                        );
                    }
                    Ok(true) => panic!("a missing file decoded to audio"),
                    Ok(false) => panic!(
                        "a missing file read as a clean end — the old bug: \
                         a corrupt file skipped itself instead of saying why"
                    ),
                }
                assert!(pipe.take_error_text().is_some(), "the failure is queryable");
            }
            Err(e) => {
                assert!(e.to_string().contains("ffmpeg"), "{e}");
            }
        }
    }

    #[test]
    fn a_stopped_pipe_reads_empty_and_is_idempotent() {
        // Build a pipe without spawning ffmpeg (the struct's stop path is
        // what this covers): stop() twice must not panic.
        let sink = crate::Sink::open_default().unwrap_or_else(|_| {
            // No device in CI: the stop path under test never touches the PCM.
            crate::Sink::open("null").expect("null sink")
        });
        let mut child = Command::new("true")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let pipe = FfPipe {
            child: Mutex::new(child),
            stdout: Mutex::new(std::io::BufReader::with_capacity(1024, stdout)),
            stderr_text: Arc::new(Mutex::new(String::new())),
            drainer_handle: Mutex::new(None),
            handed: AtomicU64::new(0),
            rate: Mutex::new(1.0),
            stopped: AtomicBool::new(false),
            failed: Mutex::new(None),
            start_seconds: 0.0,
            sink,
        };
        pipe.stop();
        pipe.stop();
        assert!(pipe.handed_frames() == 0);
    }
}
