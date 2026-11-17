//! The audiobook session: a decoded ffmpeg stream, its writer thread, and
//! the position that survives a restart.
//!
//! A TTS session synthesises chunks on demand; an audiobook session decodes
//! one continuous stream. The writer loop is VLC's `alsa.c` injection thread
//! translated to this app's shape:
//!
//! * **The ring paces the writes.** VLC's thread writes whenever the ring has
//!   room and never sleeps a computed interval; the queue's depth *is* the
//!   pacing, and the device's `delay()` is the clock. The old loop slept one
//!   period per write, which measured an 83 ms position error and left only
//!   ~85 ms of slack: any ffmpeg hiccup longer than that was an audible gap.
//!   This loop feeds until the ring holds [`QUEUE_TARGET_SECONDS`] and waits
//!   on the *device* when it is full — a 2.5 s producer stall rides out
//!   inaudibly, measured, where 85 ms of slack XRUNed.
//! * **Position in file seconds is the played edge**, not the handed edge:
//!   the sink's `written − delay`, translated by the atempo rate. A bookmark
//!   from the handed edge runs a whole ring ahead of the ears — at the 2 s
//!   ring, two seconds into the future per save.
//! * **Pause is the device's own `snd_pcm_pause`.** The ring stays intact —
//!   up to two seconds of decoded audio survive the pause instead of being
//!   thrown away and re-fetched — and the sink's paused playhead arm holds
//!   the position still. Resuming is one call, not a re-spawn of the pipe.
//!   (VLC prefers the same hw pause and only falls back to drop when the
//!   device cannot.)
//! * **`finished` fires after the tail is heard.** VLC drains and reports
//!   `aout_DrainedReport` when the *ring* empties, not when the decoder
//!   stops: EOF here arms `finished`, the loop writes out what is left, and
//!   the flag is set once the sink's queue has played to the last frame.
//!   Without the wait, auto-advance cut the last ring-depth of every
//!   chapter — silent for a 340 ms ring, a murdered cliff-hanger at 2 s.
//! * **A seek is a respawn**: the pipe's start position is fixed at spawn,
//!   so seeking stops the device and starts a new one from the new offset.
//!   That is the honest shape of a seek with a child process: exact, and
//!   costing one spawn (~0.3 s to first audio on the user's 22-hour book).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use sn_media::ffpipe::{FfPipe, PipeSpec};

use crate::TtsError;

/// How full the writer loop keeps the sink's ring, in seconds.
///
/// VLC's aout keeps up to `AOUT_MAX_PREPARE_TIME` (2 s) of audio queued
/// ahead of the playhead; the same target here covers an ffmpeg
/// spawn-to-first-audio of ~0.3 s with room to spare, and a deeper queue
/// only adds seek/pause latency.
const QUEUE_TARGET_SECONDS: f64 = 2.0;

/// The decay floor of the sink's queue: one period of frames sits in the
/// device's buffer permanently once the stream runs dry (measured: delay
/// settles at ~1–2 periods and never reaches zero on this box's PipeWire
/// plugin). Waiting for a true zero would hang the tail forever.
const PERIOD_RESIDUE_SECONDS: f64 = 0.08;

/// How long the tail wait may run before it declares the chapter finished
/// anyway. Two seconds of queue decays in two seconds of wall clock on a
/// healthy device; the timeout only fires on a device whose delay report
/// never falls below the residue floor, and the cost of the false finish is
/// at most a couple of unheard periods — inaudible, versus auto-advance
/// wedged forever, which is not.
const TAIL_WAIT_TIMEOUT: Duration = Duration::from_secs(8);

/// One decode poll, in frames. Sized to fill a `QUEUE_TARGET` ring from an
/// empty pipe in a handful of iterations without over-allocating.
const FEED_FRAMES: usize = 8192;

/// A playing audiobook: the pipe, its writer thread, and the position.
pub struct AudiobookSession {
    device: Arc<AudiobookDevice>,
    stop_flag: Arc<AtomicBool>,
    writer: Option<std::thread::JoinHandle<()>>,
}

/// The pipe plus the state the writer loop and the UI both read.
///
/// The type keeps the `AudiobookDevice` name the UI already knows. Unlike a
/// TTS `Device` — a sink the caller hands frames to — this *is* the frame
/// source, and the loop pulls decoded audio out of the pipe into the sink.
pub struct AudiobookDevice {
    pipe: FfPipe,
    /// The offset in the file this stream was opened at.
    chapter_start: f64,
    stopped: AtomicBool,
    /// Mirrored by the session's `paused`; the device owns the authoritative
    /// flag because the writer thread reads it between feeds.
    paused: AtomicBool,
    /// Set when the decoder has delivered everything and the sink has
    /// *played* it out — the VLC `aout_DrainedReport` moment. Never set by a
    /// timeout, which is just ffmpeg being slow to fill.
    finished: AtomicBool,
    /// Set only by a decode failure (the child died, the pipe errored). The
    /// UI must tell this listener "the file broke" rather than skip a
    /// chapter as if it had ended.
    failed: std::sync::Mutex<Option<String>>,
}

impl AudiobookDevice {
    /// Open the stream at `chapter_start` in the file.
    pub fn open(
        path: &std::path::Path,
        chapter_start: f64,
        rate: f64,
    ) -> Result<Self, TtsError> {
        let pipe = FfPipe::spawn(&PipeSpec {
            path: path.to_path_buf(),
            start_seconds: chapter_start,
            rate,
        })
        .map_err(TtsError::Audio)?;
        Ok(Self {
            pipe,
            chapter_start,
            stopped: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            failed: std::sync::Mutex::new(None),
        })
    }

    /// The audible playhead in file seconds — the sink's played edge, not the
    /// decode edge.
    pub fn file_seconds(&self) -> f64 {
        self.pipe.file_seconds()
    }

    /// The rate this stream plays at.
    pub fn rate(&self) -> f64 {
        self.pipe.rate()
    }

    /// The file offset this stream started at.
    pub fn chapter_start(&self) -> f64 {
        self.chapter_start
    }

    /// Whether the stream has played out to its very end.
    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Relaxed)
    }

    /// Why the stream died, if it died. `Some` exactly once.
    pub fn take_error(&self) -> Option<String> {
        self.failed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    /// Pause on the device, keeping the decoded queue intact.
    ///
    /// A device that cannot pause fails the call; the session then keeps the
    /// pause flag only, and the writer stops feeding (the room-first check in
    /// `FfPipe::feed` sees the full ring and waits). The queue survives;
    /// `close()` on the session is what drops the sink.
    pub fn pause(&self, paused: bool) -> Result<(), TtsError> {
        self.paused.store(paused, Ordering::Relaxed);
        // Pausing the writer's feeding is not enough: the ring holds up to
        // two seconds that must not keep playing after the button. The
        // device's own pause holds the queue at its depth, and the sink's
        // paused playhead arm freezes the position with it.
        self.pipe.sink().pause(paused).map_err(TtsError::Audio)
    }

    /// Stop the stream and the sink. Idempotent.
    pub fn stop(&self) {
        if self.stopped.swap(true, Ordering::Relaxed) {
            return;
        }
        self.paused.store(false, Ordering::Relaxed);
        // The sink is stopped with the pipe: a stop the listener asked for
        // must leave nothing playing (the sink contract — "audio cannot start
        // playing again after stop returns"), and `sink.stop` also rewinds
        // `written` to the played edge, so the position left behind is the
        // honest heard edge rather than a queue that keeps decaying.
        self.pipe.stop();
        self.pipe.sink().stop();
    }
}

impl AudiobookSession {
    /// Open `path` from `start_seconds` at `rate` and start playing.
    pub fn start(
        path: &std::path::Path,
        start_seconds: f64,
        rate: f64,
    ) -> Result<Self, TtsError> {
        let device = Arc::new(AudiobookDevice::open(path, start_seconds, rate)?);
        let stop_flag = Arc::new(AtomicBool::new(false));
        let writer = {
            let device = device.clone();
            let stop = stop_flag.clone();
            std::thread::spawn(move || {
                // VLC's injection loop, pulled one level up: feed the ring
                // whenever it has room below the target, wait on the device
                // when it is full, and let the queue's depth pace the stream.
                // No wall-clock sleep paces anything — the old loop's
                // per-period sleep measured an 83 ms position error and left
                // 85 ms of stall slack; this shape measured 0.5 ms.
                loop {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    if device.paused.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(20));
                        continue;
                    }
                    // Keep the queue near the target: while the audible queue
                    // (the sink's delay) is below it, pull from the pipe. When
                    // the pipe is the thing that is slow — a spawn, a seek, a
                    // network mount waking up — `feed` simply returns less
                    // often and the ring covers the gap.
                    let queued = device.pipe.sink().delay_seconds();
                    if queued >= QUEUE_TARGET_SECONDS {
                        std::thread::sleep(Duration::from_millis(50));
                        continue;
                    }
                    match device.pipe.feed(FEED_FRAMES) {
                        Ok(true) => continue,
                        Ok(false) => {
                            // The decoder delivered everything. The chapter is
                            // not over until the *ears* have had it: wait for
                            // the queue to decay (VLC reports the end through
                            // `aout_DrainedReport`, after the ring empties),
                            // then finish. A skip here cut the last two
                            // seconds of every chapter at the new ring depth.
                            // NOT via `snd_pcm_drain`: on this box's PipeWire
                            // plugin, drain drops everything above what the
                            // plugin itself buffers — measured on the deep 2 s
                            // ring, a full queue fell to the plugin's ~7 600
                            // frames in one call and `played` never moved
                            // again. (The TTS path's shallow 16 K ring never
                            // saw this, which is why its drain-and-wait gate
                            // passes.) The queue decays on its own; the wait
                            // is the drain.
                            //
                            // A pause parks the wait (the queue is frozen
                            // while paused, so the decay resumes on wake); a
                            // stop abandons it. The deadline is the honest
                            // backstop: the residue this wait tolerates is at
                            // most a couple of periods (~40 ms, inaudible),
                            // and a device whose delay never falls that far
                            // must not wedge auto-advance forever.
                            let mut started = std::time::Instant::now();
                            loop {
                                if stop.load(Ordering::Relaxed) {
                                    break;
                                }
                                if device.paused.load(Ordering::Relaxed) {
                                     // Park: the queue is frozen with the
                                    // device, so wait for the wake-up rather
                                    // than racing its frozen delay. The
                                    // deadline restarts on wake — the pause
                                    // must not consume the tail's own wait.
                                    std::thread::sleep(Duration::from_millis(50));
                                    started = std::time::Instant::now();
                                    continue;
                                }
                                if started.elapsed() > TAIL_WAIT_TIMEOUT {
                                    break;
                                }
                                let queued = device.pipe.sink().delay_seconds();
                                if queued <= PERIOD_RESIDUE_SECONDS {
                                    break;
                                }
                                std::thread::sleep(Duration::from_millis(50));
                            }
                            if !stop.load(Ordering::Relaxed) {
                                device.finished.store(true, Ordering::Relaxed);
                            }
                            break;
                        }
                        Err(e) => {
                            // A dead decoder is not the end of a chapter. The
                            // error is surfaced once, through the device, and
                            // the UI tells the listener rather than skipping.
                            *device.failed.lock().unwrap_or_else(|p| p.into_inner()) =
                                Some(e.to_string());
                            break;
                        }
                    }
                }
            })
        };
        Ok(Self {
            device,
            stop_flag,
            writer: Some(writer),
        })
    }

    /// The audible playhead in file seconds.
    pub fn file_seconds(&self) -> f64 {
        self.device.file_seconds()
    }

    /// Whether the decoded audio has run out to the stream's end.
    pub fn is_finished(&self) -> bool {
        self.device.is_finished()
    }

    /// Why the stream failed, if it did. `Some` exactly once.
    pub fn take_error(&self) -> Option<String> {
        self.device.take_error()
    }

    pub fn is_paused(&self) -> bool {
        self.device.paused.load(Ordering::Relaxed)
    }

    /// Pause or resume. The device holds the queue where it is; the position
    /// holds with it.
    pub fn set_paused(&self, paused: bool) {
        if self.device.stopped.load(Ordering::Relaxed) {
            self.device.paused.store(paused, Ordering::Relaxed);
            return;
        }
        // The device flag is set first so the writer thread observes the
        // pause between feeds even if the PCM call below fails.
        self.device.paused.store(paused, Ordering::Relaxed);
        if let Err(e) = self.device.pause(paused) {
            tracing::debug!(error = %e, "device pause unsupported; flag-only pause");
        }
    }

    pub fn stop(&self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        self.device.stop();
    }
}

impl Drop for AudiobookSession {
    fn drop(&mut self) {
        self.stop();
        if let Some(handle) = self.writer.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one ALSA device is shared by every test in this module, and
    /// cargo's harness runs tests in parallel threads of one process. Five
    /// sessions opening five 2-second rings at once starve each other's
    /// playheads (the pause test measured a zero position under exactly that
    /// load), so the hardware tests hold this lock for their whole run.
    /// Pure unit tests in other modules are unaffected.
    static DEVICE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A short generated wav, so no book is needed.
    fn tone(path: &std::path::Path, seconds: u32) {
        let rate = 48_000u32;
        let frames = rate * seconds;
        let mut bytes = Vec::new();
        let data_len = frames * 2 * 2;
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&rate.to_le_bytes());
        bytes.extend_from_slice(&(rate * 4).to_le_bytes());
        bytes.extend_from_slice(&4u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        for i in 0..frames {
            let v =
                ((i as f64 * 440.0 / rate as f64 * std::f64::consts::TAU).sin() * 8000.0) as i16;
            bytes.extend_from_slice(&v.to_le_bytes());
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        std::fs::write(path, bytes).unwrap();
    }

    /// The stream must reach `finished` only after the sink has played out —
    /// and it must get there at all. The bug this guards on both ends: the
    /// old loop's `finished` fired at EOF, skipping the ring's unheard tail
    /// (two seconds at the new depth), and a pause or stop during the tail
    /// left the flag unset, wedging auto-advance. A pause during the tail is
    /// a *hold*, not an end; the flag stays unset only while held.
    #[test]
    fn finished_arrives_only_after_the_tail_has_played() {
        let _device = DEVICE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join("sn-abook-tail");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("tone.wav");
        tone(&wav, 2);

        let session = AudiobookSession::start(&wav, 0.0, 1.0).expect("open");
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while std::time::Instant::now() < deadline {
            if session.is_finished() {
                break;
            }
            if session.take_error().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(
            session.is_finished(),
            "a 2s stream never finished within 20s — the tail wait wedged"
        );
        assert!(session.take_error().is_none(), "a clean stream reported an error");
        // The finished position is the stream's end, not its start: the tail
        // really played.
        let end = session.file_seconds();
        assert!(
            end > 1.5,
            "finished fired with the playhead at {end:.2}s of 2.0s — the tail was skipped"
        );
    }

    /// A pause must hold the position and survive a resume; the decoded
    /// queue stays intact (nothing re-fetches), and the playhead does not
    /// move while paused.
    #[test]
    fn pause_holds_the_position_and_resume_continues() {
        let _device = DEVICE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join("sn-abook-pause");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("tone.wav");
        tone(&wav, 6);

        let session = AudiobookSession::start(&wav, 0.0, 1.0).expect("open");
        // Let the playhead actually reach the audible edge before pausing:
        // the sink's played counter only advances once the device is Running,
        // and under a loaded test box that can take longer than the sleep.
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            std::thread::sleep(Duration::from_millis(100));
            let at = session.file_seconds();
            if at > 0.5 || std::time::Instant::now() > deadline {
                break;
            }
        }
        session.set_paused(true);
        let at_pause = session.file_seconds();
        assert!(at_pause > 0.2, "paused too early to observe: {at_pause}");
        std::thread::sleep(Duration::from_millis(600));
        let held = session.file_seconds();
        assert!(
            (held - at_pause).abs() < 0.15,
            "the playhead moved {at_pause:.2}→{held:.2} while paused"
        );
        session.set_paused(false);
        std::thread::sleep(Duration::from_millis(400));
        let after = session.file_seconds();
        assert!(
            after > held + 0.05,
            "resume did not continue: {held:.2}→{after:.2}"
        );
        session.stop();
    }

    /// A LONG pause (past `feed`'s room-timeout window, ~12 s) must not fake
    /// an EOF: the old code read the room timeout as the stream's end, set
    /// `finished` on resume, and auto-advance skipped to the next chapter —
    /// a lunch break truncated the book. The stream must still be alive and
    /// unfinished after a long pause, and must continue on wake.
    #[test]
    fn a_long_pause_does_not_fake_an_end() {
        let _device = DEVICE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join("sn-abook-longpause");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // 30 s: long enough that after a 13 s pause there is audio left.
        let wav = dir.join("tone.wav");
        tone(&wav, 30);

        let session = AudiobookSession::start(&wav, 0.0, 1.0).expect("open");
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            std::thread::sleep(Duration::from_millis(100));
            if session.file_seconds() > 0.5 || std::time::Instant::now() > deadline {
                break;
            }
        }
        session.set_paused(true);
        let at_pause = session.file_seconds();
        // Longer than feed's 40 × (250 ms wait + 50 ms paused sleep) window.
        std::thread::sleep(Duration::from_millis(13_000));
        assert!(
            !session.is_finished(),
            "a paused stream reported itself finished — the room timeout read as EOF"
        );
        assert!(session.take_error().is_none(), "a pause recorded as a failure");
        session.set_paused(false);
        // Give the resumed writer a moment; the position must keep moving
        // past where the pause left it (the paused playhead arm holds
        // position, so `at_pause` is the floor to beat — not 0).
        std::thread::sleep(Duration::from_millis(800));
        let resumed = session.file_seconds();
        assert!(
            resumed > at_pause + 0.05,
            "resume after a long pause did not continue: {at_pause:.2}→{resumed:.2}"
        );
        assert!(
            !session.is_finished(),
            "a 30s stream finished right after resuming — the tail wait fired early"
        );
        session.stop();
    }

    /// A stop during playback must not read as the stream's end: the UI
    /// advances a chapter only on a real finish, never on a close.
    #[test]
    fn a_stop_is_not_a_finish_and_not_an_error() {
        let _device = DEVICE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join("sn-abook-stop");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("tone.wav");
        tone(&wav, 4);

        let session = AudiobookSession::start(&wav, 0.0, 1.0).expect("open");
        std::thread::sleep(Duration::from_millis(500));
        session.stop();
        std::thread::sleep(Duration::from_millis(200));
        assert!(!session.is_finished(), "a hand stop reported as the stream's end");
        assert!(session.take_error().is_none(), "a hand stop reported as a failure");
    }

    /// A corrupt file must surface its error rather than read as a chapter
    /// that ended: the old pipe swallowed ffmpeg's stderr, so a bad file
    /// skipped itself silently.
    #[test]
    fn a_broken_file_is_an_error_not_an_end() {
        let _device = DEVICE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join("sn-abook-broken");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bad = dir.join("bad.m4b");
        std::fs::write(&bad, [0u8; 4096]).unwrap();

        let session = AudiobookSession::start(&bad, 0.0, 1.0).expect("spawn succeeds");
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let mut error = None;
        while std::time::Instant::now() < deadline {
            if let Some(e) = session.take_error() {
                error = Some(e);
                break;
            }
            if session.is_finished() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let error = error.expect("a corrupt file read as a clean end or hung");
        assert!(
            !error.is_empty(),
            "the error should carry ffmpeg's reason, not an empty string"
        );
        assert!(
            !session.is_finished(),
            "a decode failure must not flag the stream finished"
        );
    }

    /// The requested rate must reach the decoder: 0.5x delivers twice the
    /// samples for the same file, and the audible position maps back to the
    /// real file position.
    #[test]
    fn the_requested_rate_reaches_the_decoder() {
        let _device = DEVICE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join("sn-abook-rate");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("tone.wav");
        tone(&wav, 2);

        for (rate, expected_ratio) in [(1.0, 1.0), (0.5, 2.0), (2.0, 0.5)] {
            // Driven through the session, the way playback really runs: a
            // tight feed loop would race the sink (a write failing mid-loop
            // ends the read silently), while the session's own loop is the
            // contract under test — the rate survives prefs → pipe → ears.
            let session = AudiobookSession::start(&wav, 0.0, rate).expect("open");
            let deadline = std::time::Instant::now() + Duration::from_secs(30);
            while std::time::Instant::now() < deadline {
                if session.is_finished() || session.take_error().is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            assert!(
                session.take_error().is_none(),
                "rate {rate}: the decode failed: {:?}",
                session.take_error()
            );
            assert!(session.is_finished(), "rate {rate}: the stream never finished");
            // `handed_frames` counts interleaved FRAMES; at the pipe's 48 kHz
            // the seconds are frames/48000. (The old `/2` halved everything:
            // the counter moved from samples to frames and the test kept the
            // sample-era divisor.)
            let seconds = session.device.pipe.handed_frames() as f64 / 48_000.0;
            let want = 2.0 * expected_ratio;
            assert!(
                (seconds - want).abs() < 0.25,
                "rate {rate} produced {seconds:.3}s of audio for a 2s file, expected ~{want:.3}s"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
