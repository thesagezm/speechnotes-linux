//! The audio sink: ALSA playback with exact sample accounting.
//!
//! This is the substrate TTS sits on, so the one thing it must get right is
//! **position**. A player that reports the wrong position mis-highlights
//! read-along, mis-saves the resume bookmark, and mis-seeks on pause. The
//! number is therefore not "bytes written" — that runs ahead of what is
//! audible — but samples handed to the device *minus* what the device has not
//! played yet.
//!
//! Under PipeWire (which this box runs, and which every Wayland desktop routes
//! ALSA through) that lag is `snd_pcm_delay()` while the stream runs — and a
//! frozen lie once it stops: an underrun or a drain pins the reported value
//! forever. The state gate in [`Sink::position`] encodes that measurement.
//!
//! Blocking is explicit and bounded, but not zero: the producer waits in
//! [`Sink::wait_for`] *before* generating a chunk, and `write` blocks for at
//! most about a period on a healthy device. A suspended device can block a
//! write longer — that is the device applying backpressure. Readers never
//! queue behind a write: [`Sink::position`] takes the lock only if it is free,
//! and answers from the last known playhead otherwise.

pub mod ffpipe;
pub mod resampler;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use alsa::device_name::HintIter;
use alsa::pcm::{Access, Format, HwParams, PCM};
use alsa::{Direction, ValueOr};

pub use resampler::Resampler;

/// The format everything is played in.
///
/// S16LE is not a default, it is a decision: every TTS engine in this family
/// produces signed 16-bit PCM, and converting to float32 on the way to the
/// device would cost a copy per buffer for no benefit. 48 kHz stereo is what
/// the PipeWire sinks on this box negotiate, and a device that cannot do
/// exactly this fails at [`Sink::open`] — audibly resampling the *device
/// stream* to fit a sound card is how a wrong pitch gets shipped. Engines at
/// other rates are converted by [`Resampler`] before the sink, never by ALSA.
pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u16 = 2;

/// The errno ALSA reports for an underrun. Matched by number, not by a nix
/// re-export: `alsa` documents that its nix version can move under a minor
/// release, which would break the import but never the number.
#[cfg(target_os = "linux")]
const EPIPE: i32 = 32;

/// Buffer sizes in frames. One period drives ALSA's interrupt rate; the buffer
/// holds several so a synth that stalls for a millisecond does not underrun.
pub const PERIOD_FRAMES: u32 = 1_024;
pub const BUFFER_FRAMES: u32 = 16_384;

/// Why playback could not start, or a write could not proceed.
#[derive(Debug)]
pub enum AudioError {
    NoDevice(&'static str),
    /// The device cannot play [`SAMPLE_RATE`]/[`CHANNELS`] S16LE.
    Format(String),
    /// The buffer handed to [`Sink::write`] is not whole frames.
    BadInput(&'static str),
    /// The sink was stopped; a stopped sink refuses further writes — open a
    /// new one for the next stream.
    Stopped,
    Alsa(String),
    /// The decoder itself failed — a corrupt file, an unsupported codec, a
    /// dead child process. Distinct from `Alsa` so the UI names the file's
    /// problem instead of blaming the sound card.
    Decode(String),
}

impl std::fmt::Display for AudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AudioError::NoDevice(why) => write!(f, "no audio device: {why}"),
            AudioError::Format(e) => write!(f, "the device cannot play the requested format: {e}"),
            AudioError::BadInput(why) => write!(f, "bad write buffer: {why}"),
            AudioError::Stopped => write!(
                f,
                "the sink was stopped and refuses further writes; open a new sink for the next stream"
            ),
            AudioError::Alsa(e) => write!(f, "audio device error: {e}"),
            AudioError::Decode(e) => write!(f, "audio decode failed: {e}"),
        }
    }
}

impl std::error::Error for AudioError {}

/// Where the samples came from and where they have got to.
///
/// `written` is monotonic and never decreases; `played` is derived from it and
/// the device's lag, and is what a reader should show. Keeping both means a
/// glitch — an underrun, a device reset — cannot make a position jump backwards
/// without `written` saying so.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Position {
    pub written: u64,
    pub played: u64,
}

impl Position {
    /// Seconds of audio, at the rate the caller's stream runs at.
    ///
    /// Takes the rate rather than assuming [`SAMPLE_RATE`]: the value is
    /// frames, and a bookmark computed at the wrong rate is wrong by the ratio
    /// — silent, and permanent once stored.
    pub fn seconds(&self, device_rate: u32) -> f64 {
        self.written as f64 / device_rate as f64
    }

    /// True once everything written has been played.
    pub fn finished(&self) -> bool {
        self.written > 0 && self.written == self.played
    }
}

/// A device opened for playback, with the buffer accounting this module exists
/// to get right.
///
/// One stream at a time: the synth thread that feeds it must follow the
/// wait-then-write discipline documented on [`Sink::write`], and after
/// [`Sink::stop`] the sink is finished — a new stream opens a new sink.
pub struct Sink {
    pcm: Mutex<PCM>,
    written: AtomicU64,
    played: AtomicU64,
    /// Underruns recovered by [`Sink::wait_for`] or [`Sink::write`]. Each one
    /// is a potential
    /// audible click, so it is counted, not swallowed: the smoke gate fails on
    /// any, and Batch 5's stall metrics read it.
    underruns: AtomicU64,
    /// Set by [`Sink::stop`] while holding the pcm lock, checked by
    /// [`Sink::write`] under the same lock — the pairing that makes "audio
    /// plays after stop returned" impossible rather than merely unlikely.
    stopped: AtomicBool,
    /// Set when a write fails, so the error surfaces once instead of being
    /// logged on every buffer for the rest of the session.
    failed: Mutex<Option<String>>,
}

impl Sink {
    /// Open the default playback device at [`SAMPLE_RATE`]/[`CHANNELS`] S16LE.
    ///
    /// "default" is the name ALSA itself resolves through
    /// `/etc/asound.conf` and `~/.asoundrc`, so this is the desktop's chosen
    /// output rather than a guess. `alsa 0.9` has no `default_device_name()`.
    pub fn open_default() -> Result<Self, AudioError> {
        Self::open("default")
    }

    /// Open the default device with a ring `buffer_frames` deep.
    ///
    /// The TTS path stays on [`Self::open_default`] — a 16 384-frame ring
    /// (~340 ms) is right for a voice that wants a short time-to-first-audio
    /// while still riding out a synth stall. A decoded
    /// audiobook wants the opposite: VLC sizes its ALSA ring from
    /// `AOUT_MAX_ADVANCE_TIME` (~3 s) precisely so a stalled producer — for
    /// this app, an ffmpeg child mid-spawn — is inaudible. Measured on this
    /// box: a 96 256-frame (2 s) ring is negotiated exactly as asked, and a
    /// 2.5 s producer stall rides out without an underrun where the 16 384
    /// ring XRUNs a second before.
    pub fn open_default_sized(buffer_frames: u32) -> Result<Self, AudioError> {
        Self::open_sized("default", buffer_frames)
    }

    /// Open a named device, e.g. `"default"`, `"pipewire"` or a card name.
    pub fn open(device: &str) -> Result<Self, AudioError> {
        Self::open_sized(device, BUFFER_FRAMES)
    }

    /// Open a named device with an explicit ring depth in frames.
    pub fn open_sized(device: &str, buffer_frames: u32) -> Result<Self, AudioError> {
        // `nonblock: false`. The blocking open is deliberate: a non-blocking
        // handle makes every write return `EAGAIN` and turns a latency problem
        // into a busy loop. The pipeline blocks in `wait_for` instead, where the
        // wait is intentional.
        let pcm = PCM::new(device, Direction::Playback, false)
            .map_err(|e| AudioError::Alsa(format!("{device}: {e}")))?;
        {
            let params = default_params(&pcm, buffer_frames).map_err(alsa_err)?;
            pcm.hw_params(&params).map_err(alsa_err)?;
            // The stream starts itself: with the start threshold at one
            // period, the first writei arms it and playback begins from that
            // first chunk. An explicit `start()` here was tried and was wrong
            // — starting an *empty* ring is an immediate underrun, a race
            // with the first write that the underruns counter kept catching
            // at the start of every run. Auto-start is also the low-latency
            // choice: sound begins one period (~21 ms) after the first chunk,
            // not after a full ring has filled.
            let sw = pcm.sw_params_current().map_err(alsa_err)?;
            sw.set_start_threshold(PERIOD_FRAMES as i64).map_err(alsa_err)?;
            pcm.sw_params(&sw).map_err(alsa_err)?;
        }
        // The format is a requirement, not a request: `set_rate` used
        // `ValueOr::Nearest`, so a device that cannot do 48 kHz would have
        // negotiated something nearby and every pitch would be wrong by the
        // ratio. Verify what actually stuck.
        let (rate, channels) = {
            let params = pcm.hw_params_current().map_err(alsa_err)?;
            (
                params.get_rate().map_err(alsa_err)?,
                params.get_channels().map_err(alsa_err)?,
            )
        };
        if rate != SAMPLE_RATE || channels != CHANNELS as u32 {
            return Err(AudioError::Format(format!(
                "{device} negotiated {rate} Hz × {channels}, wanted {SAMPLE_RATE} × {CHANNELS}"
            )));
        }
        Ok(Self {
            pcm: Mutex::new(pcm),
            written: AtomicU64::new(0),
            played: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
            stopped: AtomicBool::new(false),
            failed: Mutex::new(None),
        })
    }

    /// The device's actual rate and channel count.
    ///
    /// Asked of the *hardware params*, not assumed: what a report prints
    /// should be what the device does, not what the caller asked for.
    pub fn negotiated(&self) -> Result<(u32, u16), AudioError> {
        let pcm = self.pcm.lock().unwrap_or_else(|e| e.into_inner());
        let params = pcm.hw_params_current().map_err(alsa_err)?;
        let rate = params.get_rate().map_err(alsa_err)?;
        let channels = params.get_channels().map_err(alsa_err)? as u16;
        Ok((rate, channels))
    }

    /// The device's ALSA state name, for diagnostics.
    ///
    /// A frozen position is unreadable without it: "played stopped moving"
    /// could be a paused stream, an underrun or a drained device, and each
    /// wants a different response. Logged on stalls, never on the hot path.
    pub fn state_name(&self) -> &'static str {
        let pcm = self.pcm.lock().unwrap_or_else(|e| e.into_inner());
        state_name(pcm.state())
    }

    /// Interleaved i16 frames, already at the device rate.
    ///
    /// The blocking call the synth thread makes, under one discipline, which
    /// is also what keeps the accounting exact: call
    /// [`wait_for`](Sink::wait_for) for the period first, then write at most
    /// that much. A write larger than the free space can be *partially*
    /// committed by ALSA before it reports an underrun, and the retry below
    /// would replay the committed prefix — the discipline makes that case
    /// impossible rather than handled. Returns how many frames the device
    /// took, which is what `written` counts.
    pub fn write(&self, frames: &[i16]) -> Result<usize, AudioError> {
        check_whole_frames(frames.len())?;
        debug_assert!(
            frames.len() <= BUFFER_FRAMES as usize * CHANNELS as usize,
            "write larger than the device ring; follow the wait_for discipline"
        );
        let pcm = self.pcm.lock().unwrap_or_else(|e| e.into_inner());
        if self.stopped.load(Ordering::Relaxed) {
            return Err(AudioError::Stopped);
        }
        let io = pcm.io_i16().map_err(alsa_err)?;
        match io.writei(frames) {
            Ok(frames) => {
                self.written.fetch_add(frames as u64, Ordering::Relaxed);
                Ok(frames)
            }
            Err(e) => {
                // EPIPE is underrun: the device's buffer ran dry because a
                // chunk was late. Recover once, then count what was accepted,
                // so `written` and `played` stay comparable — the gate's "no
                // gaps" claim is a measurement of this recovery. The count is
                // the honest part: a recovery can click, and "no clicks" is a
                // measurement, not an assumption.
                if e.errno() == EPIPE {
                    self.underruns.fetch_add(1, Ordering::Relaxed);
                    match pcm.prepare().and_then(|()| io.writei(frames)) {
                        Ok(frames) => {
                            self.written.fetch_add(frames as u64, Ordering::Relaxed);
                            Ok(frames)
                        }
                        Err(e) => Err(self.record(e)),
                    }
                } else {
                    Err(self.record(e))
                }
            }
        }
    }

    /// Seconds of audio queued in the device ring and not yet played — the
    /// number VLC's injection thread implicitly paces against, and the
    /// writer loop's "how far ahead of the listener am I" answer. Zero means
    /// everything handed over has been heard.
    pub fn delay_seconds(&self) -> f64 {
        let pcm = self.pcm.lock().unwrap_or_else(|e| e.into_inner());
        match pcm.state() {
            alsa::pcm::State::Running | alsa::pcm::State::Draining | alsa::pcm::State::Prepared => {
                match pcm.delay() {
                    Ok(delay) if delay > 0 => delay as f64 / f64::from(SAMPLE_RATE),
                    _ => 0.0,
                }
            }
            // Paused: the queue is frozen, not empty — report its depth.
            alsa::pcm::State::Paused | alsa::pcm::State::Suspended => match pcm.delay() {
                Ok(delay) if delay > 0 => delay as f64 / f64::from(SAMPLE_RATE),
                _ => 0.0,
            },
            _ => 0.0,
        }
    }

    /// Underruns recovered since the sink was opened.
    /// Counted at both recovery sites — [`Sink::wait_for`]'s XRun branch,
    /// where the wait-then-write discipline routes every stall-induced
    /// underrun, and [`Sink::write`]'s EPIPE path for out-of-discipline
    /// callers. Each one is a potential audible click, so it is counted, not
    /// swallowed: the smoke gate fails on any, and Batch 5's stall metrics
    /// read it.
    pub fn underruns(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }

    /// Wait until at least `frames` are free in the device ring.
    ///
    /// The synth thread calls this between chunks: it is the only place the
    /// pipeline is allowed to block, and it blocks *before* generating rather
    /// than after, so no audio is ever generated that has nowhere to go. This
    /// wait-then-write rhythm is also what keeps [`Sink::write`]'s accounting
    /// exact — see the discipline note there.
    ///
    /// Polled rather than `snd_pcm_wait`, on purpose: under the PipeWire ALSA
    /// plugin `wait` returns ready while the ring is still nearly full (its
    /// poll fd signals on graph activity, not on space), which let writes race
    /// a second of audio into the plugin's buffer ahead of playback and starve
    /// the hardware stream at the end. `avail_update` is the number that does
    /// not lie, so the loop polls it and sleeps between looks; the wakeups are
    /// nothing next to a period of synthesis.
    pub fn wait_for(&self, frames: usize, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let ready = {
                let pcm = self.pcm.lock().unwrap_or_else(|e| e.into_inner());
                if self.stopped.load(Ordering::Relaxed) {
                    return false;
                }
                match pcm.avail_update() {
                    Ok(n) => n as usize >= frames,
                    Err(_) => {
                        // An underrun while waiting: recover so the next write
                        // succeeds, and call the space available — write()
                        // refills from the prepare point. Counted here, not in
                        // write(): under the wait-then-write discipline this
                        // branch is where every stall-induced underrun lands,
                        // because by the time write() runs the stream is
                        // Prepared again and writei never sees EPIPE.
                        if pcm.state() == alsa::pcm::State::XRun {
                            self.underruns.fetch_add(1, Ordering::Relaxed);
                            let _ = pcm.prepare();
                            true
                        } else {
                            false
                        }
                    }
                }
            };
            if ready {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn record(&self, e: alsa::Error) -> AudioError {
        let msg = e.to_string();
        *self.failed.lock().unwrap_or_else(|e| e.into_inner()) = Some(msg.clone());
        AudioError::Alsa(msg)
    }

    /// An error seen since the last call, for surfacing once to the user.
    pub fn take_error(&self) -> Option<String> {
        self.failed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    /// Refresh and return the position.
    ///
    /// `played = written - lag` while the stream runs. When it is not running,
    /// the lag answer depends on *why*, and each state gets its own truth:
    ///
    /// * `XRun` or `Setup` — everything written was played or discarded by the
    ///   underrun; the position is everything.
    /// * `Suspended` — nothing is being played, so the playhead must not move;
    ///   report the last known position rather than pretend the queue drained.
    ///
    /// The lock is taken only if it is free: a producer stalled mid-write on a
    /// stuck device must not freeze the UI thread reading the playhead.
    pub fn position(&self) -> Position {
        let last = self.played.load(Ordering::Relaxed);
        // `written` is read under the pcm lock, the same lock under which
        // write() adds to it and stop() rewinds it: a reader descheduled
        // across a stop must not install the pre-rewind count as the playhead.
        // A bookmark saved from that reading would start the voice ahead of
        // what was heard — the write-ahead-bookmark bug TRANSITION.md §6.5
        // bans — so the read is not hoisted above the lock even though the
        // atomic alone would allow it.
        // A paused or suspended device must not read `written − delay()`: a
        // paused PipeWire PCM keeps answering `delay()` with the frozen queue
        // depth (measured: the same frame count across the pause), so the
        // subtraction reads ~a ring BEHIND what the listener heard and a
        // bookmark saved from it would rewind. VLC's paused stream holds its
        // last clock position the same way. One cheap state check decides
        // whether the device read is worth taking at all — and it is a
        // `try_lock`, not a `lock`: a producer stalled mid-write on a stuck
        // device must not freeze the UI thread reading the playhead (the
        // contract at the top of this method). Contended means "not known
        // running"; the caller answers from the last position.
        let running = match self.pcm.try_lock() {
            Ok(pcm) => matches!(
                pcm.state(),
                alsa::pcm::State::Running | alsa::pcm::State::Draining
            ),
            Err(std::sync::TryLockError::Poisoned(poison)) => matches!(
                poison.into_inner().state(),
                alsa::pcm::State::Running | alsa::pcm::State::Draining
            ),
            // Contended — the second try_lock below will also fail and the
            // last-known pair is returned; no need to touch the device.
            Err(std::sync::TryLockError::WouldBlock) => false,
        };
        let observed = if running {
            match self.pcm.try_lock() {
                Ok(pcm) => playhead(&pcm, self.written.load(Ordering::Relaxed), last),
                // Poisoned by a panic elsewhere: recover the guard the way every
                // other lock in this module does.
                Err(std::sync::TryLockError::Poisoned(poison)) => {
                    playhead(&poison.into_inner(), self.written.load(Ordering::Relaxed), last)
                }
                // Contended — a write is in flight, possibly stuck on a suspended
                // device. The last answer stands rather than queueing behind
                // it, and `written` reads as `last` rather than the live
                // atomic: a stop() racing this call rewinds `written` under
                // the lock, and the unlocked read could pair the pre-rewind
                // count with a stale `played`. `last` is conservative and
                // self-consistent; the next uncontended call heals it.
                Err(std::sync::TryLockError::WouldBlock) => {
                    return Position {
                        written: last,
                        played: last,
                    }
                }
            }
        } else {
            last
        };
        // Monotonic, and installed with a compare-exchange so two concurrent
        // readers cannot each clamp against a stale `last` and transiently
        // regress the stored playhead.
        let mut current = last;
        while observed > current {
            match self.played.compare_exchange_weak(
                current,
                observed,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
        let played = self.played.load(Ordering::Relaxed);
        // Read after the CAS for the same reason as above: a rewind that
        // raced this call must not be half-reflected in the pair.
        let written = self.written.load(Ordering::Relaxed);
        Position { written, played }
    }

    /// Play out everything buffered and leave the device idle.
    ///
    /// Two ways a stream ends, and drain handles both: still `Running`, ALSA's
    /// own drain plays the ring out; already `XRun` (the common case — the
    /// device consumed the last frame before anyone thought to ask), the state
    /// is reset so the next write starts clean. Best-effort either way: a
    /// device that refuses is reported through [`Self::take_error`], not a
    /// panic, because the audio has already been heard either way.
    ///
    /// **A drained stream has played everything it was given.** That is what
    /// draining means, and the position has to say so: the playhead is only
    /// read from a *running* device (see [`Self::position`]), and once the
    /// drain returns the device sits in `Setup` with an empty queue. Without
    /// this the playhead stayed frozen at its last running sample — measured
    /// on this box: 24 149 of 48 000 frames, half the audio — so
    /// `Position::finished()` was never true, the TTS session could only ever
    /// end `Stopped`, and the book walk (which advances on `Finished` alone)
    /// never moved. The playhead is installed as `written` here, under the
    /// same lock the write path takes, so a write in flight cannot race the
    /// claim.
    pub fn drain(&self) {
        let pcm = self.pcm.lock().unwrap_or_else(|e| e.into_inner());
        let mut drained = false;
        match pcm.state() {
            alsa::pcm::State::Running => {
                match pcm.drain() {
                    Ok(()) => drained = true,
                    Err(e) => {
                        *self.failed.lock().unwrap_or_else(|e| e.into_inner()) = Some(e.to_string());
                    }
                }
            }
            alsa::pcm::State::XRun => {
                // The device had already consumed or discarded everything it
                // was given; there is nothing left to play out.
                let _ = pcm.prepare();
                drained = true;
            }
            _ => {}
        }
        // The stream is over: everything handed to the device has been heard.
        // Only the states that say so are claimed — a suspended or paused
        // device was holding audio nobody heard, and claiming it as played
        // would move the bookmark past words the listener missed.
        let state = pcm.state();
        let heard_everything = drained
            && matches!(
                state,
                alsa::pcm::State::Setup
                    | alsa::pcm::State::Prepared
                    | alsa::pcm::State::Running
                    | alsa::pcm::State::Draining
                    | alsa::pcm::State::XRun
            );
        if heard_everything {
            let written = self.written.load(Ordering::Relaxed);
            let mut current = self.played.load(Ordering::Relaxed);
            while written > current {
                match self.played.compare_exchange_weak(
                    current,
                    written,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(actual) => current = actual,
                }
            }
        }
    }

    /// Pause the stream where it is, or resume it.
    ///
    /// `snd_pcm_pause` holds the ring intact — the queue keeps its depth and
    /// `delay` keeps answering with the same number — where a stop/drop pair
    /// throws away up to a ring of audio the listener never heard and would
    /// have to be re-fetched from the decoder on resume. PipeWire's ALSA
    /// plugin reports `can_pause` on this box and honours it (measured: delay
    /// holds at the same frame count across the pause, and writes land in the
    /// queue while paused), so this is the primary arm, not a fallback. A
    /// device that cannot pause fails the call and the caller keeps its
    /// current discipline.
    pub fn pause(&self, paused: bool) -> Result<(), AudioError> {
        let pcm = self.pcm.lock().unwrap_or_else(|e| e.into_inner());
        if self.stopped.load(Ordering::Relaxed) {
            return Ok(());
        }
        pcm.pause(paused).map_err(alsa_err)
    }

    /// Whether the stream is currently paused on the device.
    pub fn is_paused(&self) -> bool {
        let pcm = self.pcm.lock().unwrap_or_else(|e| e.into_inner());
        pcm.state() == alsa::pcm::State::Paused
    }

    /// Drop everything not yet played, for stop.
    ///
    /// `written` is rewound to what the device has played, so the position does
    /// not claim audio exists that was discarded. The sink is then finished:
    /// `stopped` is set *while holding the pcm lock*, so a producer whose
    /// `write` is interleaved with this call either committed before the drop
    /// (its frames are accounted, then discarded and `written` rewound over
    /// them) or is refused by the flag afterwards — audio cannot start playing
    /// again after stop returns. A new stream opens a new sink; the handle is
    /// cheap and reopening re-negotiates a device that may have changed.
    pub fn stop(&self) {
        // The rewind is computed *under* the pcm lock, from the device's own
        // state and with consumption paused first. Sampling the atomic after
        // the fact had a race: a concurrent position() in the transient
        // Prepared-after-recovery window installs `written` as played — up to
        // a ring of frames this call is about to discard — and the rewind
        // would preserve the inflation. Holding the lock across the whole
        // sequence shuts position() out of that window.
        let played = {
            let pcm = self.pcm.lock().unwrap_or_else(|e| e.into_inner());
            let written = self.written.load(Ordering::Relaxed);
            let last = self.played.load(Ordering::Relaxed);
            let _ = pcm.pause(true);
            let played = playhead(&pcm, written, last);
            // `snd_pcm_drop` discards the device's queued frames; `prepare`
            // puts the stream back into a state that accepts writes. Both are
            // best-effort — a device already in that state errors, and that is
            // not worth reporting.
            //
            // The `drop` name collides with `Drop::drop`, which Rust forbids
            // calling explicitly, so it is reached through a function pointer
            // rather than a method call. It is the crate's own public method
            // (`alsa::pcm::PCM::drop`), not the destructor.
            let drop_frames: fn(&alsa::PCM) -> alsa::Result<()> = alsa::PCM::drop;
            let _ = drop_frames(&pcm);
            let _ = pcm.prepare();
            self.stopped.store(true, Ordering::Relaxed);
            played
        };
        self.written.store(played, Ordering::Relaxed);
    }
}

/// The playhead, from the device's own state.
///
/// The delay report is only meaningful while a stream is actually playing:
/// an underrun (`XRun`) or a completed drain (`Setup`) pins PipeWire's
/// reported lag at its last value forever, so reading it there freezes the
/// position 142 ms short of the end. Each state gets its own truth:
///
/// * `Running` / `Draining` — `written − delay()` is the honest playhead.
/// * `Paused` — the device is holding the ring exactly as it was; the playhead
///   must not move, and `delay()` keeps answering with the frozen queue depth,
///   so the honest reading is the last known position rather than a live one.
/// * `XRun` / `Setup` — everything written was played or discarded by the
///   underrun; the position is everything.
/// * `Suspended` — nothing is being played, so the playhead must not move;
///   hold the last known position rather than pretend the queue drained.
fn playhead(pcm: &PCM, written: u64, last: u64) -> u64 {
    match pcm.state() {
        alsa::pcm::State::Running | alsa::pcm::State::Draining => match pcm.delay() {
            Ok(delay) => written.saturating_sub(delay.max(0) as u64),
            // A device that cannot say leaves `written` as the position:
            // early by at most one buffer rather than wrong by an amount
            // that grows.
            Err(_) => written,
        },
        alsa::pcm::State::Paused | alsa::pcm::State::Suspended => last,
        // Prepared/Setup with unplayed content still reads as fully played —
        // bounded by one period (~21 ms, a sub-period write in flight) and
        // healed by the next honest sample. Exactness here would need appl/hw
        // pointer arithmetic ALSA does not expose portably.
        _ => written,
    }
}

/// Whole interleaved frames only, or the write is refused before ALSA sees it.
///
/// `writei` floors a buffer to whole frames silently, so a pipeline bug at the
/// mono→stereo seam would drip one sample per call with no signal — an
/// inaudible drift that corrupts every position downstream. The check lives in
/// a free function so it is testable without a device.
fn check_whole_frames(samples: usize) -> Result<(), AudioError> {
    if samples % CHANNELS as usize != 0 {
        return Err(AudioError::BadInput(
            "buffer is not a whole number of frames",
        ));
    }
    Ok(())
}

fn state_name(state: alsa::pcm::State) -> &'static str {
    match state {
        alsa::pcm::State::Setup => "setup",
        alsa::pcm::State::Prepared => "prepared",
        alsa::pcm::State::Running => "running",
        alsa::pcm::State::XRun => "underrun",
        alsa::pcm::State::Draining => "draining",
        alsa::pcm::State::Suspended => "suspended",
        alsa::pcm::State::Disconnected => "disconnected",
        _ => "open",
    }
}

fn default_params<'a>(pcm: &'a PCM, buffer_frames: u32) -> alsa::Result<HwParams<'a>> {
    let params = HwParams::any(pcm)?;
    params.set_access(Access::RWInterleaved)?;
    params.set_format(Format::s16())?;
    params.set_channels(CHANNELS as u32)?;
    params.set_rate(SAMPLE_RATE, ValueOr::Nearest)?;
    params.set_period_size_near(PERIOD_FRAMES as i64, ValueOr::Nearest)?;
    params.set_buffer_size_near(buffer_frames as i64)?;
    Ok(params)
}

fn alsa_err(e: alsa::Error) -> AudioError {
    AudioError::Alsa(e.to_string())
}

/// The playback devices ALSA knows about, for a settings pane.
///
/// Names come from ALSA's own hint iterator rather than a hand-written list, so
/// a device that appears is listed without a code change and one that goes away
/// disappears. A machine with no hints (no card, only a `pipewire` PCM) yields
/// the fallback names the app can still open.
pub fn playback_devices() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // `HintIter` yields `Hint`, not `Result<Hint>`: the null-termination check
    // is inside its `next`, so a machine that answers with nothing simply ends
    // the iteration.
    if let Ok(hints) = HintIter::new(None, &std::ffi::CString::new("pcm").unwrap()) {
        for hint in hints {
            if let Some(name) = hint.name {
                if !out.iter().any(|d| *d == name) {
                    out.push(name);
                }
            }
        }
    }
    for fallback in ["default", "pipewire", "pulse"] {
        if !out.iter().any(|d| d == fallback) {
            out.push(fallback.to_string());
        }
    }
    out
}

/// Convert mono i16 frames to interleaved stereo by duplication.
///
/// Engines emit mono; the device is stereo. Done once, on the way out, and not
/// with an upmixer: duplicating the sample is what a mono voice already is, and
/// anything cleverer here would change what the user hears.
pub fn mono_to_stereo(mono: &[i16]) -> Vec<i16> {
    let mut out = Vec::with_capacity(mono.len() * 2);
    for s in mono {
        out.push(*s);
        out.push(*s);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_position_converts_to_seconds_at_the_stream_rate() {
        let p = Position {
            written: SAMPLE_RATE as u64 * 3,
            played: 0,
        };
        assert!((p.seconds(SAMPLE_RATE) - 3.0).abs() < 1e-9);
        // At any other rate the same frames mean different seconds: a bookmark
        // computed at the wrong rate is wrong by the ratio.
        assert!((p.seconds(24_000) - 6.0).abs() < 1e-9);
    }

    #[test]
    fn nothing_written_is_not_finished() {
        assert!(!Position::default().finished());
        assert!(Position { written: 1, played: 1 }.finished());
        assert!(!Position { written: 2, played: 1 }.finished());
    }

    #[test]
    fn a_device_list_is_never_empty() {
        // Even on a machine with no hints, the fallbacks must be there: a
        // settings pane offering no device at all is indistinguishable from a
        // bug.
        assert!(!playback_devices().is_empty());
        assert!(playback_devices().contains(&"default".to_string()));
        assert!(playback_devices().contains(&"pipewire".to_string()));
    }

    #[test]
    fn errors_name_the_device_they_came_from() {
        let e = AudioError::Alsa("Broken pipe".into());
        assert!(e.to_string().contains("Broken pipe"));
        assert!(AudioError::NoDevice("no default device")
            .to_string()
            .contains("no default device"));
        assert!(AudioError::Stopped.to_string().contains("stopped"));
        assert!(AudioError::BadInput("not whole frames")
            .to_string()
            .contains("not whole frames"));
    }

    #[test]
    fn mono_becomes_stereo_by_duplication() {
        assert_eq!(mono_to_stereo(&[1, -1]), vec![1, 1, -1, -1]);
        assert!(mono_to_stereo(&[]).is_empty());
        // The result is interleaved pairs, which is what the device wants.
        assert_eq!(mono_to_stereo(&[7]).len() % 2, 0);
    }

    #[test]
    fn whole_frame_check_rejects_a_dripped_sample() {
        // Three samples at two channels is one and a half frames: `writei`
        // would floor it and drip a sample per call. The error must happen
        // here, at the seam, not as an inaudible drift.
        assert!(check_whole_frames(6).is_ok());
        assert!(check_whole_frames(5).is_err());
        assert!(check_whole_frames(0).is_ok());
    }

    #[test]
    fn the_open_format_contract_is_enforced() {
        // `open` must fail loudly on a device that negotiated a different
        // format — resampling the device stream to fit a card ships a wrong
        // pitch. This test pins the error surface without a device: the
        // message carries both numbers so the log explains itself.
        let e = AudioError::Format("usb-speaker negotiated 44100 Hz × 2, wanted 48000 × 2".into());
        let msg = e.to_string();
        assert!(msg.contains("44100") && msg.contains("48000"));
    }
}
