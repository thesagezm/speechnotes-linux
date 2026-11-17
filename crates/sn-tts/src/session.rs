//! The playback pipeline: a synth thread one chunk ahead of an audio thread.
//!
//! Shape of a session:
//!
//! ```text
//! synth thread                     audio thread
//! ────────────                     ────────────
//! for chunk in plan:               for msg in channel:
//!   engine.synthesize(text)          pause/stop check
//!   resample → stereo   ──chan──►    period writes (wait_for → write)
//!                                    frontier ← played frames, mapped
//!                                      through the chunk frame-history
//!                                    (on failure: soft beep, frontier ← end)
//!                                  drain → Finished
//! ```
//!
//! The channel is a bounded `sync_channel` — that bound *is* the
//! generation-ahead: at most `channel_depth` chunks are synthesised beyond
//! the one sounding, so a fast engine can never run away from playback.
//!
//! The frontier is the number the whole family trusts, and the ring is the
//! trap it avoids: on a real device, `written − played` sits at the ring
//! depth (~340 ms at 48 kHz) for the whole run, and the frames written most
//! recently belong to the *previous* chunk's tail. Converting the played
//! count through the current chunk's geometry alone therefore bookmarks up
//! to half a sentence of never-heard audio — the write-ahead bookmark bug
//! (TRANSITION §6.5) reintroduced one level down. So the audio thread keeps
//! a history of each chunk's frame range in the device's cumulative stream,
//! and every conversion — per-period for read-along, stop-time for the
//! bookmark — walks that history: each played frame is attributed to the
//! chunk it actually belongs to. `pause` stops the sink (rewinding
//! `written` to `played`) and converts the stopped count, so the pause point
//! is exactly what was heard. `resume` re-plans from that byte offset, so
//! the partial sentence is finished, not replayed from its start.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sn_media::{mono_to_stereo, Sink, CHANNELS, PERIOD_FRAMES, SAMPLE_RATE};

use crate::beep::soft_beep;
use crate::engine::Engine;
use crate::plan::{SpeechPlan, MAX_CHUNK_CHARS};
use crate::schedule;
use crate::TtsError;

/// How long to wait for the device between period writes before giving up.
/// Short enough that stop feels immediate, long enough that a brief device
/// hiccup does not fail the session.
const WAIT_TIMEOUT: Duration = Duration::from_millis(250);

/// A session's lifecycle. `Finished`, `Stopped` and `Failed` are terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayState {
    Playing,
    Paused,
    Finished,
    Stopped,
    Failed,
}

impl PlayState {
    pub fn is_terminal(self) -> bool {
        matches!(self, PlayState::Finished | PlayState::Stopped | PlayState::Failed)
    }
}

/// One chunk's measured cost. `chars` is the chunk's character count, or the
/// text that *would* have been spoken when `beeped` is true (synthesis
/// failed; the chunk's slot in the audio is the soft beep).
#[derive(Debug, Clone)]
pub struct ChunkMetric {
    pub index: usize,
    pub chars: usize,
    pub beeped: bool,
    /// Time the engine spent synthesising.
    pub synth: Duration,
    /// Time the audio takes to play.
    pub audio: Duration,
}

impl ChunkMetric {
    /// Real-time factor: synthesis seconds per audio second. Under 1.0 keeps
    /// up with playback; the gate reports this number rather than asserting
    /// speed.
    pub fn rtf(&self) -> f64 {
        if self.audio.as_secs_f64() <= 0.0 {
            return 0.0;
        }
        self.synth.as_secs_f64() / self.audio.as_secs_f64()
    }
}

/// A finished session's numbers, for `--speak` and the gate.
#[derive(Debug, Clone)]
pub struct Metrics {
    pub state: PlayState,
    pub ttfa: Option<Duration>,
    pub frontier: usize,
    pub errors: usize,
    pub chunks: Vec<ChunkMetric>,
    ///  the byte a resumed session re-planned from —
    /// the heard edge at the pause. Zero when the session never resumed.
    pub resume_frontier: usize,
}

impl Metrics {
    pub fn synth_total(&self) -> Duration {
        self.chunks.iter().map(|c| c.synth).sum()
    }

    pub fn audio_total(&self) -> Duration {
        self.chunks.iter().map(|c| c.audio).sum()
    }

    /// Mean RTF across every real (non-beeped) chunk with audio.
    pub fn mean_rtf(&self) -> f64 {
        let real: Vec<&ChunkMetric> = self
            .chunks
            .iter()
            .filter(|c| !c.beeped && c.audio.as_secs_f64() > 0.0)
            .collect();
        if real.is_empty() {
            return 0.0;
        }
        real.iter().map(|c| c.rtf()).sum::<f64>() / real.len() as f64
    }
}

/// A snapshot of where a session is, taken under the run lock. The UI polls
/// this; the bookmark is written from `frontier` and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    pub state: PlayState,
    /// Byte offset of the played edge in the source text.
    pub frontier: usize,
    /// Index, in the *current* plan, of the chunk now sounding. A resumed
    /// session re-plans from the frontier, so indices move — match them
    /// against the plan for the current `plan_epoch`.
    pub audible_chunk: usize,
    /// Time from the session's start to its first audible sample. Measured
    /// once per session, not per resume: it is the start-up latency metric.
    pub ttfa: Option<Duration>,
    pub synth_total: Duration,
    pub audio_total: Duration,
    pub errors: usize,
    pub chunks_done: usize,
    pub underruns: u64,
    /// The reason, when state is `Failed`.
    pub error: Option<String>,
}

// `Progress::frontier_before_join` has a single call site (the pause/resume
// shape test below). An earlier body returned `Metrics::frontier` — the FINAL
// edge — which made the caller's tail-plan arithmetic come out empty (3 + 0
// instead of 3 + 5) and the test red. The caller's semantics pin the honest
// answer: the edge the session re-planned from when it resumed, which
// `Metrics::resume_frontier` now carries.
impl Progress {
    // Test-only: the production read of the played edge is `Metrics::frontier`
    // itself; this helper exists for the pause/resume shape test.
    #[cfg(test)]
    fn frontier_before_join(&self, m: &Metrics) -> usize {
        m.resume_frontier
    }
}


#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// ALSA device name; `"default"` is the desktop's chosen output.
    pub device: String,
    /// How many chunks the synth thread may run ahead of playback. The
    /// anticipation budget: at 2 one slow synthesis (a heavy chunk, a page's
    /// engine work landing late) drained the bank and the device starved.
    /// 4 keeps roughly a minute of audio banked for the neural engines — the
    /// iOS app's render-ahead rule in chunk terms — while bounding memory to
    /// a few chunk buffers.
    pub channel_depth: usize,
    /// Pin the opener's character budget instead of asking the schedule.
    /// `None` — production — asks [`crate::schedule`], which sizes the opener
    /// in audio seconds from what the engine measured last session. Tests and
    /// the headless gate pin it: a plan's chunk shape must be reproducible
    /// across runs, and the schedule's learned state is per engine and per
    /// process.
    pub pinned_opener_chars: Option<usize>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            device: "default".to_string(),
            channel_depth: 4,
            pinned_opener_chars: None,
        }
    }
}

#[derive(Debug)]
struct RunState {
    state: PlayState,
    frontier: usize,
    audible_chunk: usize,
    ttfa: Option<Duration>,
    synth_total: Duration,
    audio_total: Duration,
    errors: usize,
    /// Cumulative across incarnations, so a resumed session's accounting
    /// covers everything the listener heard.
    chunks_done: usize,
    underruns: u64,
    error: Option<String>,
    ///  the byte the session re-planned from when
    /// `resume` ran — the heard edge at the pause. Zero while the session
    /// has never resumed. Metrics carries it out so a caller can rebuild
    /// the tail plan exactly as the session did.
    resume_frontier: usize,
}

struct Shared {
    run: Mutex<RunState>,
    metrics: Mutex<Vec<ChunkMetric>>,
}

impl Shared {
    fn commit(&self, f: impl FnOnce(&mut RunState)) {
        let mut run = self.run.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut run);
    }

    /// The current state, for threads that only need to check one field.
    fn state(&self) -> PlayState {
        self.run.lock().unwrap_or_else(|e| e.into_inner()).state
    }

    /// The current frontier, for threads that need one field.
    fn frontier(&self) -> usize {
        self.run.lock().unwrap_or_else(|e| e.into_inner()).frontier
    }
}

/// Where one chunk sits in the device's cumulative frame stream.
///
/// The device's `played` counter runs over a single continuous stream made
/// of every chunk this sink incarnation has written, in order. A played
/// count is converted to a source byte offset by finding the chunk whose
/// frame range contains it — never by assuming it is inside the chunk whose
/// writes began most recently, which is false by the ring's depth.
#[derive(Debug, Clone, Copy)]
struct FrameRange {
    written_start: u64,
    /// Exclusive. `written_start + total frames of the chunk`.
    written_end: u64,
    byte_start: usize,
    byte_len: usize,
    index: usize,
}

/// The ranges of chunks still close enough behind the playhead to matter.
/// Bounded: a position lagging more than the keep-list is impossible (the
/// ring is a few hundred milliseconds; a chunk is never shorter than a
/// beep), and the bound keeps a paused-forever session's memory flat.
const HISTORY_KEEP: usize = 64;

#[derive(Default)]
struct FrameHistory {
    ranges: VecDeque<FrameRange>,
}

impl FrameHistory {
    fn push(&mut self, range: FrameRange) {
        self.ranges.push_back(range);
        while self.ranges.len() > HISTORY_KEEP {
            self.ranges.pop_front();
        }
    }

    /// The byte offset and chunk index a cumulative played count corresponds
    /// to. A count past everything written (the drained tail) maps to the
    /// last chunk's end; the fallback for an empty history is the caller's
    /// current frontier, unchanged.
    fn convert(&self, played: u64, fallback: usize) -> (usize, usize) {
        for r in self.ranges.iter().rev() {
            if played >= r.written_start {
                let total = (r.written_end - r.written_start) as usize;
                let in_chunk = (played.min(r.written_end) - r.written_start) as usize;
                let offset = (in_chunk * r.byte_len) / total.max(1);
                return (r.byte_start + offset.min(r.byte_len), r.index);
            }
        }
        (fallback, 0)
    }
}

/// The slice of the audio device the pipeline uses, as a trait.
///
/// The real sink is `sn_media::Sink`. The trait exists so the tests can
/// inject a device whose `played` count *lags* `written` by a ring's worth
/// of frames — the one property of real hardware the ALSA null device (and
/// therefore every device-free test) cannot reproduce, and the property the
/// bookmark correctness stands on.
pub(crate) trait Device: Send {
    fn position(&self) -> sn_media::Position;
    fn wait_for(&self, frames: usize, timeout: Duration) -> bool;
    fn write(&self, frames: &[i16]) -> Result<usize, sn_media::AudioError>;
    fn underruns(&self) -> u64;
    fn take_error(&self) -> Option<String>;
    fn drain(&self);
    fn stop(&self);
    /// The device's own name for its current state, for the completion log.
    /// A stream that ends `Stopped` instead of `Finished` is nearly always a
    /// device-state question, and the state is the whole answer.
    fn state_name(&self) -> &'static str;
}

impl Device for Sink {
    fn position(&self) -> sn_media::Position {
        Sink::position(self)
    }
    fn wait_for(&self, frames: usize, timeout: Duration) -> bool {
        Sink::wait_for(self, frames, timeout)
    }
    fn write(&self, frames: &[i16]) -> Result<usize, sn_media::AudioError> {
        Sink::write(self, frames)
    }
    fn underruns(&self) -> u64 {
        Sink::underruns(self)
    }
    fn take_error(&self) -> Option<String> {
        Sink::take_error(self)
    }
    fn drain(&self) {
        Sink::drain(self)
    }
    fn stop(&self) {
        Sink::stop(self)
    }
    fn state_name(&self) -> &'static str {
        Sink::state_name(self)
    }
}

/// What a chunk's writes ended with, so the audio thread can react once.
enum Advance {
    Completed,
    Paused,
    Stopped,
    Failed(String),
}

/// A live playback session. Shared with the UI behind an `Arc`; all methods
/// take `&self`, and every thread holds its own clone of the inner arcs.
pub struct SessionCore {
    /// The source text, kept so `resume` can re-plan from the frontier.
    text: String,
    plan: Mutex<SpeechPlan>,
    plan_epoch: AtomicU64,
    engine: Arc<dyn Engine>,
    cfg: SessionConfig,
    shared: Arc<Shared>,
    /// "Everything aborts": set by `stop()` and by the audio thread on a
    /// fatal device error, checked by both threads. Also set when a paused
    /// session is stopped, so a synth thread still inside a slow engine
    /// winds down instead of outliving the session.
    stop_flag: Arc<AtomicBool>,
    pause_flag: Arc<AtomicBool>,
    handles: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

impl SessionCore {
    /// Start speaking `text` from `resume_byte`.
    ///
    /// The sink is opened here so a dead device is an immediate `Err` the UI
    /// can show, not an async `Failed` nobody noticed. An empty plan is not
    /// an error: the session is born `Finished`.
    pub fn start(
        text: &str,
        resume_byte: usize,
        engine: Arc<dyn Engine>,
        cfg: SessionConfig,
    ) -> Result<Arc<SessionCore>, TtsError> {
        // The plan comes first: an empty one is born `Finished` without
        // opening — and possibly failing on — a device there is nothing to
        // speak through. The opener's budget comes from the schedule, which
        // sizes it in audio seconds from what this engine measured last
        // session (and falls back to the fixed cap until then) — unless the
        // config pins it, which is how tests and `--speak` keep a plan's
        // chunk shape reproducible.
        let opener = cfg
            .pinned_opener_chars
            .unwrap_or_else(|| schedule::opener_budget(engine.name(), engine.rate(), MAX_CHUNK_CHARS));
        let plan = SpeechPlan::build_with_opener(text, resume_byte, opener);
        let sink: Option<Box<dyn Device>> = if plan.is_empty() {
            None
        } else {
            Some(Box::new(Sink::open(&cfg.device)?))
        };
        Self::begin(text, resume_byte, engine, cfg, plan, sink)
    }

    #[cfg(test)]
    fn start_with_device(
        text: &str,
        resume_byte: usize,
        engine: Arc<dyn Engine>,
        cfg: SessionConfig,
        sink: Box<dyn Device>,
    ) -> Result<Arc<SessionCore>, TtsError> {
        // The pin decides the opener exactly as `start` does: the schedule is
        // learned state shared by every test in this binary, and a plan whose
        // shape depends on it is not reproducible.
        let opener = cfg
            .pinned_opener_chars
            .unwrap_or_else(|| schedule::opener_budget(engine.name(), engine.rate(), MAX_CHUNK_CHARS));
        let plan = SpeechPlan::build_with_opener(text, resume_byte, opener);
        Self::begin(text, resume_byte, engine, cfg, plan, Some(sink))
    }

    #[allow(clippy::too_many_arguments)]
    fn begin(
        text: &str,
        resume_byte: usize,
        engine: Arc<dyn Engine>,
        cfg: SessionConfig,
        plan: SpeechPlan,
        sink: Option<Box<dyn Device>>,
    ) -> Result<Arc<SessionCore>, TtsError> {
        let frontier = if plan.is_empty() { 0 } else { resume_byte.min(plan.end_byte()) };
        let shared = Arc::new(Shared {
            run: Mutex::new(RunState {
                state: if plan.is_empty() {
                    PlayState::Finished
                } else {
                    PlayState::Playing
                },
                frontier,
                audible_chunk: 0,
                ttfa: None,
                synth_total: Duration::ZERO,
                audio_total: Duration::ZERO,
                errors: 0,
                chunks_done: 0,
                underruns: 0,
                error: None,
                resume_frontier: 0,
            }),
            metrics: Mutex::new(Vec::new()),
        });
        let core = Arc::new(Self {
            text: text.to_string(),
            plan: Mutex::new(plan),
            plan_epoch: AtomicU64::new(0),
            engine,
            cfg,
            shared,
            stop_flag: Arc::new(AtomicBool::new(false)),
            pause_flag: Arc::new(AtomicBool::new(false)),
            handles: Mutex::new(Vec::new()),
        });
        if !core.current_plan().is_empty() {
            core.spawn_threads(sink.expect("a device was opened for a non-empty plan"))?;
        }
        Ok(core)
    }

    /// The plan this session is currently speaking from, with the epoch to
    /// cache it against. The UI reads chunk boundaries from here for
    /// read-along — one derivation, never a second parser.
    pub fn plan(&self) -> (u64, SpeechPlan) {
        (self.plan_epoch.load(Ordering::SeqCst), self.current_plan())
    }

    /// The plan's generation, without copying the plan. The UI caches chunk
    /// boundaries against this and re-fetches only when it moves.
    pub fn plan_epoch(&self) -> u64 {
        self.plan_epoch.load(Ordering::SeqCst)
    }

    fn current_plan(&self) -> SpeechPlan {
        self.plan
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn progress(&self) -> Progress {
        let run = self.shared.run.lock().unwrap_or_else(|e| e.into_inner());
        Progress {
            state: run.state,
            frontier: run.frontier,
            audible_chunk: run.audible_chunk,
            ttfa: run.ttfa,
            synth_total: run.synth_total,
            audio_total: run.audio_total,
            errors: run.errors,
            chunks_done: run.chunks_done,
            underruns: run.underruns,
            error: run.error.clone(),
        }
    }

    /// Pause at what was actually heard.
    ///
    /// The audio thread observes the flag at its next write boundary (at
    /// most a period plus one [`WAIT_TIMEOUT`]), stops the sink, and
    /// converts the stopped position through the frame history — so the
    /// recorded byte is what was heard, never what was still in the ring.
    pub fn pause(&self) {
        let state = self.progress().state;
        if state.is_terminal() || state == PlayState::Paused {
            return;
        }
        self.pause_flag.store(true, Ordering::SeqCst);
    }

    /// Resume a paused session from the heard position.
    ///
    /// A new sink, a new thread pair, a plan rebuilt from the frontier byte —
    /// the chunk the listener was inside continues from where it stopped, and
    /// nothing that was heard is heard again.
    pub fn resume(&self) -> Result<(), TtsError> {
        if self.progress().state != PlayState::Paused {
            return Err(TtsError::State(
                "resume is only valid while a session is paused".to_string(),
            ));
        }
        // Replace the plan before un-pausing: a spawned thread reads it.
        let frontier = self.progress().frontier;
        let opener = self.cfg.pinned_opener_chars.unwrap_or_else(|| {
            schedule::opener_budget(self.engine.name(), self.engine.rate(), MAX_CHUNK_CHARS)
        });
        let plan = SpeechPlan::build_with_opener(&self.text, frontier, opener);
        // A pause at the very end (the final drain was interrupted) leaves
        // nothing to speak: resume is completion, without opening a device
        // to prove it.
        if plan.is_empty() {
            self.shared.commit(|r| r.state = PlayState::Finished);
            return Ok(());
        }
        self.resume_with(
            Sink::open(&self.cfg.device)
                .map(|s| Box::new(s) as Box<dyn Device>)
                .map_err(TtsError::from),
            plan,
        )
    }

    /// The tail of [`Self::resume`], split so tests can inject the device
    /// result. The device is opened *before* the state flip: on failure the
    /// session must be `Failed` — terminal, so the player detaches — never
    /// `Playing` with no threads, which nothing can ever steer out.
    fn resume_with(
        &self,
        sink: Result<Box<dyn Device>, TtsError>,
        plan: SpeechPlan,
    ) -> Result<(), TtsError> {
        let sink = match sink {
            Ok(s) => s,
            Err(e) => {
                self.shared.commit(|r| {
                    r.state = PlayState::Failed;
                    r.error = Some(e.to_string());
                });
                return Err(e);
            }
        };
        *self.plan.lock().unwrap_or_else(|e| e.into_inner()) = plan;
        self.plan_epoch.fetch_add(1, Ordering::SeqCst);

        self.pause_flag.store(false, Ordering::SeqCst);
        self.shared.commit(|r| r.state = PlayState::Playing);
        self.spawn_threads(sink)
    }

    /// Stop for good. Terminal: a stopped session is finished with its sink,
    /// and any write a producer still attempts goes nowhere.
    pub fn stop(&self) {
        let state = self.progress().state;
        if state.is_terminal() {
            return;
        }
        // Always arm the flag, even from Paused: a synth thread can still be
        // inside a slow engine call, and it must wind down.
        self.stop_flag.store(true, Ordering::SeqCst);
        if state == PlayState::Paused {
            // No audio thread is running to observe the flag; the transition
            // is made here, or a paused session could never reach a terminal
            // state and the UI would wait for it forever.
            self.shared.commit(|r| r.state = PlayState::Stopped);
        }
    }

    /// Wait for every thread to exit and collect the metrics. Used by
    /// headless runs (`--speak`); a UI session instead polls `progress` and
    /// drops the core when it turns terminal. The metrics are *taken*: a
    /// second call returns empty metrics.
    pub fn join(&self) -> Metrics {
        let handles = {
            let mut h = self.handles.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *h)
        };
        for h in handles {
            let _ = h.join();
        }
        let run = self.shared.run.lock().unwrap_or_else(|e| e.into_inner());
        let chunks = std::mem::take(
            &mut *self
                .shared
                .metrics
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        );
        Metrics {
            state: run.state,
            ttfa: run.ttfa,
            frontier: run.frontier,
            errors: run.errors,
            chunks,
            resume_frontier: run.resume_frontier,
        }
    }

    // -- internals ----------------------------------------------------------

    fn spawn_threads(&self, sink: Box<dyn Device>) -> Result<(), TtsError> {
        // The sink arrives already open — `start`/`resume` open it on the
        // caller's thread so a dead device is an error the caller sees, not
        // an async state change it has to poll for.
        //
        // Threads from earlier incarnations have exited by construction —
        // `Paused` is committed by the audio thread after it stops its sink —
        // but their handles are only reaped here.
        self.reap_finished();
        let fail = |e: TtsError| {
            // Half-spawned sessions are worse than failed ones: commit a
            // terminal state so the player can let go.
            self.shared.commit(|r| {
                r.state = PlayState::Failed;
                r.error = Some(e.to_string());
            });
            e
        };

        let plan = self.current_plan();
        let frontier = self.progress().frontier;
        self.shared
            .commit(|r| r.resume_frontier = frontier);
        let first = plan.chunks.partition_point(|c| c.end() <= frontier);
        let started = Instant::now();

        let depth = self.cfg.channel_depth.max(1);
        let (tx, rx) = mpsc::sync_channel::<Result<ChunkAudio, SynthFailure>>(depth);

        let engine = self.engine.clone();
        let plan = SpeechPlan {
            chunks: plan.chunks[first.min(plan.len())..].to_vec(),
        };
        let stop = self.stop_flag.clone();
        let pause = self.pause_flag.clone();
        let shared = self.shared.clone();

        let synth = std::thread::Builder::new()
            .name("sn-tts-synth".into())
            .spawn(move || {
                synth_loop(plan, engine, tx, stop, pause, shared);
            })
            .map_err(|e| fail(TtsError::Io(e)))?;

        let stop = self.stop_flag.clone();
        let pause = self.pause_flag.clone();
        let shared = self.shared.clone();
        // The schedule's recording needs to know whose voice it is measuring:
        // the name keys the schedule state, the rate scales its measurement.
        let engine_name = self.engine.name();
        let engine_rate = self.engine.rate();
        let audio = std::thread::Builder::new()
            .name("sn-tts-audio".into())
            .spawn(move || {
                audio_loop(
                    sink, rx, started, stop, pause, shared, engine_name, engine_rate,
                );
            })
            .map_err(|e| fail(TtsError::Io(e)))?;

        self.handles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend([synth, audio]);
        Ok(())
    }

    fn reap_finished(&self) {
        let mut handles = self.handles.lock().unwrap_or_else(|e| e.into_inner());
        handles.retain(|h| !h.is_finished());
    }
}

/// A synthesised chunk on its way to the device.
struct ChunkAudio {
    metric: ChunkMetric,
    /// Interleaved stereo at the device rate, ready for the sink.
    frames: Vec<i16>,
    /// Byte range of the chunk in the source text.
    start: usize,
    len: usize,
}

/// A chunk the engine could not synthesise. One attempt was made; the audio
/// thread plays the soft beep over this byte range and moves on.
struct SynthFailure {
    index: usize,
    start: usize,
    len: usize,
    chars: usize,
}

fn synth_loop(
    plan: SpeechPlan,
    engine: Arc<dyn Engine>,
    tx: mpsc::SyncSender<Result<ChunkAudio, SynthFailure>>,
    stop: Arc<AtomicBool>,
    pause: Arc<AtomicBool>,
    shared: Arc<Shared>,
) {
    for chunk in &plan.chunks {
        // Abort points are per chunk: a synthesis already in flight finishes
        // and is sent, and the receiver decides whether it is still wanted.
        if stop.load(Ordering::SeqCst)
            || pause.load(Ordering::SeqCst)
            || shared.state() == PlayState::Failed
        {
            return;
        }
        match engine.synthesize(&chunk.text) {
            Ok(syn) => {
                let frames = to_device_frames(&syn);
                let audio =
                    Duration::from_secs_f64(frames.len() as f64 / 2.0 / f64::from(SAMPLE_RATE));
                let metric = ChunkMetric {
                    index: chunk.index,
                    chars: chunk.text.chars().count(),
                    beeped: false,
                    synth: syn.synth_time,
                    audio,
                };
                if tx
                    .send(Ok(ChunkAudio {
                        metric,
                        frames,
                        start: chunk.start,
                        len: chunk.len,
                    }))
                    .is_err()
                {
                    return; // receiver gone: paused out or session ended
                }
            }
            Err(e) => {
                tracing::warn!(
                    chunk = chunk.index,
                    error = %e,
                    "synthesis failed once; beeping and skipping"
                );
                let msg = Err(SynthFailure {
                    index: chunk.index,
                    start: chunk.start,
                    len: chunk.len,
                    chars: chunk.text.chars().count(),
                });
                if tx.send(msg).is_err() {
                    return;
                }
            }
        }
    }
}

/// Resample an engine's output to the device rate and widen to stereo.
fn to_device_frames(syn: &crate::engine::Synthesis) -> Vec<i16> {
    let frames = if syn.sample_rate == SAMPLE_RATE {
        syn.samples.clone()
    } else {
        let mut resampler = sn_media::Resampler::new(syn.sample_rate, SAMPLE_RATE);
        let mut out = Vec::new();
        for chunk in syn.samples.chunks(4_096) {
            out.extend(resampler.push(chunk));
        }
        out.extend(resampler.flush());
        out
    };
    mono_to_stereo(&frames)
}

/// The opener measurement the schedule learns from: the moment the opener's
/// write began, the audio it carried, and its characters.
struct OpenerWatch {
    first_write: Instant,
    audio: Duration,
    chars: usize,
    /// Whether the outcome has been reported to the schedule. A session can
    /// end before any second chunk arrives; the exit paths report it then,
    /// exactly once.
    recorded: bool,
}

fn audio_loop(
    sink: Box<dyn Device>,
    rx: mpsc::Receiver<Result<ChunkAudio, SynthFailure>>,
    started: Instant,
    stop: Arc<AtomicBool>,
    pause: Arc<AtomicBool>,
    shared: Arc<Shared>,
    engine_name: &'static str,
    engine_rate: f64,
) {
    // The frame ranges of every chunk this incarnation has written: the map
    // from device positions to source bytes. Thread-local — only the audio
    // thread converts positions.
    let mut history = FrameHistory::default();
    let mut ended_by_flags = false;
    // The opener watch: the first *real* (non-beep) chunk is the opener whose
    // fate the schedule learns from. A beep opener is a failure path, not a
    // measurement — its "audio" is a fraction of a second and would teach the
    // schedule a nonsense chars-per-second.
    let mut opener: Option<OpenerWatch> = None;
    'pump: loop {
        // Flags are checked on a timer as well as between chunks, so pause
        // and stop are honoured even while the channel sits empty.
        match rx.recv_timeout(WAIT_TIMEOUT) {
            Ok(msg) => {
                if flags_set(&stop, &pause) {
                    ended_by_flags = true;
                    break 'pump;
                }
                // The schedule's raw material: once an opener is in flight,
                // the arrival of the NEXT chunk (any chunk) measures the wait
                // from the opener's first write. Late — the second buffer
                // arrived after the opener finished *playing* — is the
                // audible gap the opener target exists to prevent. The check
                // sits after the flags test so a session paused between its
                // chunks records nothing (the pause would inflate the wait).
                let mut report_opener = false;
                if let Some(watch) = opener.as_mut() {
                    if !watch.recorded {
                        watch.recorded = true;
                        report_opener = true;
                    }
                }
                if report_opener {
                    let watch = opener.as_ref().expect("just checked");
                    let wait = watch.first_write.elapsed();
                    let late = wait > watch.audio;
                    schedule::record(
                        engine_name,
                        Some(late),
                        Some(watch.audio),
                        watch.chars,
                        Some(wait),
                        engine_rate,
                    );
                }
                match msg {
                    Ok(audio) => {
                        // The Ok arm never carries a beep — failures arrive
                        // as `Err` and become beeps in their own arm below —
                        // so the first Ok chunk is the opener.
                        if opener.is_none() {
                            opener = Some(OpenerWatch {
                                first_write: Instant::now(),
                                audio: Duration::from_secs_f64(
                                    audio.frames.len() as f64 / 2.0 / f64::from(SAMPLE_RATE),
                                ),
                                chars: audio.metric.chars,
                                recorded: false,
                            });
                        }
                        match write_chunk(&*sink, &audio, started, &stop, &pause, &shared, &mut history)
                        {
                            Advance::Completed => {}
                            Advance::Failed(e) => {
                                fail_session(&shared, &stop, e);
                                return;
                            }
                            // Chunk writes end by flag between periods; a
                            // pause that lands between chunks is handled in
                            // the flag check above.
                            Advance::Paused | Advance::Stopped => {
                                ended_by_flags = true;
                                break 'pump;
                            }
                        }
                    }
                    Err(failure) => {
                        let beep = mono_to_stereo(&soft_beep(SAMPLE_RATE));
                        let audio = ChunkAudio {
                            // The beep records itself through the ordinary
                            // write path below; it carries the failed
                            // chunk's identity so the metric says what was
                            // skipped.
                            metric: ChunkMetric {
                                index: failure.index,
                                chars: failure.chars,
                                beeped: true,
                                synth: Duration::ZERO,
                                audio: Duration::from_secs_f64(
                                    beep.len() as f64 / 2.0 / f64::from(SAMPLE_RATE),
                                ),
                            },
                            frames: beep,
                            start: failure.start,
                            len: failure.len,
                        };
                        match write_chunk(&*sink, &audio, started, &stop, &pause, &shared, &mut history)
                        {
                            Advance::Completed => {
                                shared.commit(|r| {
                                    r.errors += 1;
                                });
                            }
                            Advance::Failed(e) => {
                                fail_session(&shared, &stop, e);
                                return;
                            }
                            Advance::Paused | Advance::Stopped => {
                                ended_by_flags = true;
                                break 'pump;
                            }
                        }
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if flags_set(&stop, &pause) {
                    ended_by_flags = true;
                    break 'pump;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break 'pump,
        }
    }

    // A session that ended before any second chunk arrived still taught the
    // schedule something about its opener: `record` with no wait compares the
    // opener's audio against the wait the engine is known to take, and grows
    // the target when the opener was smaller than it. Recorded on both exit
    // paths — a stopped or paused session measured its opener honestly, even
    // though nobody heard it all.
    if let Some(watch) = opener.as_ref() {
        if !watch.recorded {
            schedule::record(
                engine_name,
                None,
                Some(watch.audio),
                watch.chars,
                None,
                engine_rate,
            );
        }
    }

    if ended_by_flags {
        // The sink's stop rewinds `written` to `played`, so the position read
        // after it is exactly what was heard — the number the bookmark gets,
        // mapped through the history so the ring's backlog lands on the
        // chunk it belongs to.
        sink.stop();
        let p = sink.position();
        let (frontier, _index) = history.convert(p.played, shared.frontier());
        let paused = pause.load(Ordering::SeqCst) && !stop.load(Ordering::SeqCst);
        shared.commit(|r| {
            r.frontier = r.frontier.max(frontier);
            r.state = if paused {
                PlayState::Paused
            } else {
                PlayState::Stopped
            };
        });
        return;
    }

    // Everything written; make the device play the tail before declaring the
    // session complete. Completion is this signal — nothing infers it.
    sink.drain();
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut drained = false;
    loop {
        if sink.position().finished() {
            drained = true;
            break;
        }
        if Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let p = sink.position();
    let (frontier, _index) = history.convert(p.played, shared.frontier());
    let stopped = stop.load(Ordering::SeqCst);
    let paused = pause.load(Ordering::SeqCst) && !stopped;
    tracing::info!(
        drained,
        device = sink.state_name(),
        written = p.written,
        played = p.played,
        lag = p.written.saturating_sub(p.played),
        underruns = sink.underruns(),
        "playback tail settled"
    );
    shared.commit(|r| {
        r.frontier = r.frontier.max(frontier);
        // `Finished` is the signal that deletes the bookmark, so it may only
        // fire when the device confirmed the tail. A device that never
        // reports finished counts as stopped: the position is kept, and the
        // worst case is a resume that replays nothing.
        r.state = if drained && !stopped && !paused {
            PlayState::Finished
        } else if paused {
            PlayState::Paused
        } else {
            PlayState::Stopped
        };
    });
}

fn flags_set(stop: &AtomicBool, pause: &AtomicBool) -> bool {
    stop.load(Ordering::SeqCst) || pause.load(Ordering::SeqCst)
}

fn fail_session(shared: &Shared, stop: &AtomicBool, error: String) {
    tracing::error!(error = %error, "playback failed");
    shared.commit(|r| {
        r.state = PlayState::Failed;
        r.error = Some(error);
    });
    // The synth thread watches for Failed state before each chunk.
    stop.store(true, Ordering::SeqCst);
}

/// Write one chunk to the device in period-sized writes, committing the
/// frontier from *played* frames after every period.
fn write_chunk(
    sink: &dyn Device,
    audio: &ChunkAudio,
    started: Instant,
    stop: &AtomicBool,
    pause: &AtomicBool,
    shared: &Shared,
    history: &mut FrameHistory,
) -> Advance {
    let total_frames = audio.frames.len() / CHANNELS as usize;
    if total_frames == 0 {
        return Advance::Completed;
    }
    let written_start = sink.position().written;
    history.push(FrameRange {
        written_start,
        written_end: written_start + total_frames as u64,
        byte_start: audio.start,
        byte_len: audio.len,
        index: audio.metric.index,
    });
    let period_samples = PERIOD_FRAMES as usize * CHANNELS as usize;
    for period in audio.frames.chunks(period_samples) {
        if stop.load(Ordering::SeqCst) {
            return Advance::Stopped;
        }
        if pause.load(Ordering::SeqCst) {
            return Advance::Paused;
        }
        if !sink.wait_for(PERIOD_FRAMES as usize, WAIT_TIMEOUT) {
            // Neither flag: the device stopped accepting samples.
            return Advance::Failed(match sink.take_error() {
                Some(e) => e,
                None => "device stopped accepting samples".to_string(),
            });
        }
        match sink.write(period) {
            Ok(_) => {}
            Err(e) => return Advance::Failed(e.to_string()),
        }
        let p = sink.position();
        let (frontier, index) = history.convert(p.played, shared.frontier());
        shared.commit(|r| {
            if r.ttfa.is_none() {
                // The start-up latency metric: session start to the first
                // sample that will actually sound.
                r.ttfa = Some(started.elapsed());
                tracing::debug!(
                    ttfa_ms = started.elapsed().as_millis() as u64,
                    "first audio reached the device"
                );
            }
            r.audible_chunk = index;
            r.frontier = r.frontier.max(frontier);
            r.underruns = sink.underruns();
        });
    }
    let metric_audio = Duration::from_secs_f64(total_frames as f64 / f64::from(SAMPLE_RATE));
    shared.commit(|r| {
        r.chunks_done += 1;
        r.audio_total += metric_audio;
        r.synth_total += audio.metric.synth;
    });
    shared
        .metrics
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(ChunkMetric {
            index: audio.metric.index,
            chars: audio.metric.chars,
            beeped: audio.metric.beeped,
            synth: audio.metric.synth,
            audio: metric_audio,
        });
    Advance::Completed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Engine, Synthesis};
    use std::collections::BTreeSet;

    /// A test engine: a quiet sine per chunk, optional failures, and
    /// per-call gates a test can pause the pipeline at.
    struct FakeEngine {
        rate: u32,
        fail_indices: Vec<usize>,
        /// Notified (call index) when a gated synthesis is about to wait.
        gate_tx: std::sync::mpsc::Sender<usize>,
        gate: Arc<(Mutex<BTreeSet<usize>>, std::sync::Condvar)>,
        calls: Mutex<Vec<usize>>,
        delay: Duration,
    }

    /// The test side of the gates.
    struct GateControl {
        gate_rx: std::sync::mpsc::Receiver<usize>,
        gate: Arc<(Mutex<BTreeSet<usize>>, std::sync::Condvar)>,
    }

    impl GateControl {
        /// The index of the next call waiting at its gate.
        fn wait_blocked(&self) -> usize {
            self.gate_rx.recv().expect("a call arrived at the gate")
        }

        fn release(&self, index: usize) {
            let (lock, cvar) = &*self.gate;
            let mut set = lock.lock().unwrap();
            set.remove(&index);
            cvar.notify_all();
        }

        /// Gate a call index *before* the session starts, so the pause or
        /// stop lands at a known point in the chunk sequence.
        fn block_call(&self, index: usize) {
            let (lock, _cvar) = &*self.gate;
            lock.lock().unwrap().insert(index);
        }
    }

    impl FakeEngine {
        fn new() -> (Arc<Self>, GateControl) {
            let (tx, rx) = std::sync::mpsc::channel();
            let gate = Arc::new((Mutex::new(BTreeSet::new()), std::sync::Condvar::new()));
            let engine = Arc::new(Self {
                rate: 22_050,
                fail_indices: Vec::new(),
                gate_tx: tx,
                gate: gate.clone(),
                calls: Mutex::new(Vec::new()),
                delay: Duration::from_millis(1),
            });
            (engine, GateControl { gate_rx: rx, gate })
        }

        fn failing(self: Arc<Self>, indices: &[usize]) -> Arc<Self> {
            // The engine reaches SessionCore::start behind an Arc, so tests
            // configure it through the still-unshared handle they hold.
            let mut me = self;
            Arc::get_mut(&mut me)
                .expect("engine not yet shared")
                .fail_indices = indices.to_vec();
            me
        }
    }

    impl Engine for FakeEngine {
        fn name(&self) -> &'static str {
            "fake"
        }

        fn synthesize(&self, _text: &str) -> Result<Synthesis, TtsError> {
            let index = {
                let mut calls = self.calls.lock().unwrap();
                let index = calls.len();
                calls.push(index);
                index
            };
            let (lock, cvar) = &*self.gate;
            let mut set = lock.lock().unwrap();
            if set.contains(&index) {
                let _ = self.gate_tx.send(index);
                while set.contains(&index) {
                    set = cvar.wait(set).unwrap();
                }
            }
            drop(set);
            if self.fail_indices.contains(&index) {
                return Err(TtsError::Synthesis(format!("call {index} failed")));
            }
            std::thread::sleep(self.delay);
            // 10 ms of audio per character.
            let frames = 480usize;
            let samples: Vec<i16> = (0..frames)
                .map(|i| {
                    let v = (i as f64 * 440.0 * 2.0 * std::f64::consts::PI / self.rate as f64)
                        .sin()
                        * 8_000.0;
                    v as i16
                })
                .collect();
            Ok(Synthesis {
                sample_rate: self.rate,
                samples,
                synth_time: self.delay,
            })
        }
    }

    fn text(n_chunks: usize) -> String {
        (0..n_chunks)
            .map(|i| format!("Sentence number {i} speaks for a while. "))
            .collect()
    }

    fn cfg() -> SessionConfig {
        // The ALSA null device consumes instantly and needs no hardware: the
        // pipeline is exercised at full speed, underruns never fire, and the
        // played position always equals what was written — exactly what the
        // tests want. Real-hardware behaviour is the `--speak` gate's job.
        SessionConfig {
            device: "null".to_string(),
            channel_depth: 2,
            // The plan's chunk shape must be reproducible: the tests' byte
            // arithmetic is pinned against `SpeechPlan::build`'s fixed opener,
            // not against whatever the schedule learned from the last test to
            // finish.
            pinned_opener_chars: Some(crate::plan::FIRST_CHUNK_CHARS),
        }
    }

    fn wait_for(timeout: Duration, f: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        f()
    }

    #[test]
    fn plays_every_chunk_to_completion_with_metrics() {
        let (engine, _gate) = FakeEngine::new();
        let source = text(6);
        let core = SessionCore::start(&source, 0, engine, cfg()).unwrap();
        let metrics = core.join();
        let plan = SpeechPlan::build(&source, 0);
        assert_eq!(metrics.state, PlayState::Finished);
        assert_eq!(metrics.errors, 0);
        assert_eq!(metrics.chunks.len(), plan.len(), "one metric per chunk");
        assert!(metrics.ttfa.is_some(), "TTFA was measured");
        assert!(metrics.mean_rtf() > 0.0);
        // The frontier sits at the end of the last chunk.
        assert_eq!(metrics.frontier, plan.end_byte());
        let indices: Vec<usize> = metrics.chunks.iter().map(|c| c.index).collect();
        assert_eq!(indices, (0..plan.len()).collect::<Vec<_>>());
    }

    #[test]
    fn an_empty_plan_is_born_finished() {
        let (engine, _gate) = FakeEngine::new();
        let core = SessionCore::start("", 0, engine, cfg()).unwrap();
        assert_eq!(core.progress().state, PlayState::Finished);
        let metrics = core.join();
        assert!(metrics.chunks.is_empty());
        assert_eq!(metrics.ttfa, None);
    }

    #[test]
    fn resume_from_a_bookmark_skips_what_came_before() {
        let (engine, _gate) = FakeEngine::new();
        let source = text(6);
        let plan = SpeechPlan::build(&source, 0);
        let resume = plan.chunks[3].start;
        let core = SessionCore::start(&source, resume, engine, cfg()).unwrap();
        let metrics = core.join();
        assert_eq!(metrics.state, PlayState::Finished);
        // Only the tail was ever synthesised — the session's own plan, built
        // from the same resume byte, is the tail.
        let tail = SpeechPlan::build(&source, resume);
        assert_eq!(metrics.chunks.len(), tail.len());
        assert_eq!(metrics.frontier, plan.end_byte());
    }

    #[test]
    fn a_failed_chunk_becomes_one_beep_and_the_session_continues() {
        let (engine, _gate) = FakeEngine::new();
        let engine = engine.failing(&[2]);
        let source = text(5);
        let plan = SpeechPlan::build(&source, 0);
        let core = SessionCore::start(&source, 0, engine, cfg()).unwrap();
        let metrics = core.join();
        assert_eq!(
            metrics.state,
            PlayState::Finished,
            "one failure must not end the session"
        );
        assert_eq!(metrics.errors, 1);
        assert_eq!(metrics.chunks.len(), plan.len());
        let beeped = &metrics.chunks[2];
        assert!(beeped.beeped);
        assert!(beeped.synth.is_zero());
        // The beep has a known length; it occupied the failed chunk's slot.
        assert!((beeped.audio.as_secs_f64() - crate::beep::BEEP_SECONDS).abs() < 0.01);
        // The frontier still reaches the end: the listener passed the gap.
        assert_eq!(metrics.frontier, plan.end_byte());
    }

    #[test]
    fn all_chunks_failing_still_finishes_by_beeping_through() {
        let (engine, _gate) = FakeEngine::new();
        let source = text(3);
        let plan = SpeechPlan::build(&source, 0);
        // Every chunk in the plan fails.
        let engine = engine.failing(&(0..plan.len()).collect::<Vec<_>>());
        let core = SessionCore::start(&source, 0, engine, cfg()).unwrap();
        let metrics = core.join();
        assert_eq!(metrics.state, PlayState::Finished);
        assert_eq!(metrics.errors, plan.len());
    }

    #[test]
    fn pause_records_where_listening_stopped_and_resume_finishes_the_plan() {
        let (engine, gate) = FakeEngine::new();
        // Block chunk 3's synthesis so the pause lands between chunks.
        gate.block_call(3);
        let source = text(8);
        let core = SessionCore::start(&source, 0, engine, cfg()).unwrap();
        assert_eq!(gate.wait_blocked(), 3);
        // Chunks 0–2 are fully written on the null device before chunk 3's
        // synthesis can even return; make that visible before pausing.
        assert!(wait_for(Duration::from_secs(5), || {
            core.progress().chunks_done == 3
        }));
        core.pause();
        gate.release(3);
        assert!(wait_for(Duration::from_secs(5), || {
            core.progress().state == PlayState::Paused
        }));

        let plan = SpeechPlan::build(&source, 0);
        let frontier = core.progress().frontier;
        assert_eq!(frontier, plan.chunks[3].start, "paused at the heard edge");

        core.resume().unwrap();
        let metrics = core.join();
        assert_eq!(metrics.state, PlayState::Finished);
        assert_eq!(metrics.errors, 0);
        // Pre-pause chunks plus the rebuilt tail: every chunk heard once. The
        // tail is a plan of its own — the opener treatment applies to its
        // first chunk too — so its shape is the tail plan's, not the
        // original's.
        let tail = SpeechPlan::build(&source, core.progress().frontier_before_join(&metrics));
        assert_eq!(metrics.chunks.len(), 3 + tail.len());
        assert_eq!(metrics.frontier, plan.end_byte());
    }

    #[test]
    fn stop_is_terminal_even_from_paused() {
        let (engine, gate) = FakeEngine::new();
        gate.block_call(1);
        let core = SessionCore::start(&text(4), 0, engine, cfg()).unwrap();
        assert_eq!(gate.wait_blocked(), 1);
        assert!(wait_for(Duration::from_secs(5), || {
            core.progress().chunks_done == 1
        }));
        core.pause();
        gate.release(1);
        assert!(wait_for(Duration::from_secs(5), || {
            core.progress().state == PlayState::Paused
        }));
        core.stop();
        assert_eq!(core.progress().state, PlayState::Stopped);
        assert!(core.progress().state.is_terminal());
    }

    #[test]
    fn stop_mid_play_lands_at_a_heard_position() {
        let (engine, gate) = FakeEngine::new();
        gate.block_call(2);
        let source = text(6);
        let core = SessionCore::start(&source, 0, engine, cfg()).unwrap();
        assert_eq!(gate.wait_blocked(), 2);
        assert!(wait_for(Duration::from_secs(5), || {
            core.progress().chunks_done == 2
        }));
        core.stop();
        gate.release(2);
        assert!(wait_for(Duration::from_secs(5), || {
            core.progress().state == PlayState::Stopped
        }));
        let plan = SpeechPlan::build(&source, 0);
        assert_eq!(core.progress().frontier, plan.chunks[2].start);
    }

    #[test]
    fn a_larger_run_completes_and_stays_bounded() {
        // The sync_channel bound is std's guarantee; what is worth exercising
        // here is a long run end to end — accounting, ordering, frontier.
        let (engine, _gate) = FakeEngine::new();
        let source = text(120);
        let core = SessionCore::start(&source, 0, engine, cfg()).unwrap();
        let metrics = core.join();
        let plan = SpeechPlan::build(&source, 0);
        assert_eq!(metrics.state, PlayState::Finished);
        assert_eq!(metrics.chunks.len(), plan.len());
        let indices: Vec<usize> = metrics.chunks.iter().map(|c| c.index).collect();
        assert_eq!(indices, (0..plan.len()).collect::<Vec<_>>());
    }

    #[test]
    fn pause_then_resume_never_repeats_heard_audio() {
        let (engine, gate) = FakeEngine::new();
        gate.block_call(2);
        let source = text(5);
        let core = SessionCore::start(&source, 0, engine, cfg()).unwrap();
        assert_eq!(gate.wait_blocked(), 2);
        assert!(wait_for(Duration::from_secs(5), || {
            core.progress().chunks_done == 2
        }));
        core.pause();
        gate.release(2);
        assert!(wait_for(Duration::from_secs(5), || {
            core.progress().state == PlayState::Paused
        }));
        let first_frontier = core.progress().frontier;
        core.resume().unwrap();
        let metrics = core.join();
        assert_eq!(metrics.state, PlayState::Finished);
        assert!(metrics.frontier >= first_frontier);
        let original = SpeechPlan::build(&source, 0);
        assert_eq!(metrics.frontier, original.end_byte());
        // The plan was rebuilt for the tail: the epoch moved off zero.
        let (epoch, resumed_plan) = core.plan();
        assert_eq!(epoch, 1);
        assert_eq!(resumed_plan.end_byte(), original.end_byte());
    }

    #[test]
    fn resume_is_only_valid_from_paused() {
        let (engine, _gate) = FakeEngine::new();
        let core = SessionCore::start(&text(2), 0, engine, cfg()).unwrap();
        assert!(matches!(core.resume(), Err(TtsError::State(_))));
        let _ = core.join();
    }

    #[test]
    fn a_missing_device_fails_the_start_immediately() {
        let (engine, _gate) = FakeEngine::new();
        let cfg = SessionConfig {
            device: "definitely-not-a-device".to_string(),
            channel_depth: 2,
            pinned_opener_chars: Some(crate::plan::FIRST_CHUNK_CHARS),
        };
        let err = match SessionCore::start(&text(2), 0, engine, cfg) {
            Err(e) => e,
            Ok(_) => panic!("a missing device must fail the start"),
        };
        assert!(matches!(err, TtsError::Audio(_)));
    }


    /// A device whose `played` count lags `written` by a fixed ring depth —
    /// the property real hardware has and the ALSA null device lacks. Every
    /// frontier claim in this crate is suspect until it survives this sink.
    struct LaggingSink {
        written: AtomicU64,
        played: AtomicU64,
        stopped: AtomicBool,
        lag: u64,
        /// The model's written counter, shared so the test can compute the
        /// heard position independently of the code under test.
        counters: Arc<AtomicU64>,
    }

    impl LaggingSink {
        fn new(lag: u64) -> (Box<dyn Device>, Arc<AtomicU64>) {
            let counters = Arc::new(AtomicU64::new(0));
            (
                Box::new(Self {
                    written: AtomicU64::new(0),
                    played: AtomicU64::new(0),
                    stopped: AtomicBool::new(false),
                    lag,
                    counters: counters.clone(),
                }),
                counters,
            )
        }
    }

    impl Device for LaggingSink {
        fn position(&self) -> sn_media::Position {
            if self.stopped.load(Ordering::SeqCst) {
                // After stop, written was rewound to played; both frozen.
                let heard = self.played.load(Ordering::SeqCst);
                sn_media::Position {
                    written: heard,
                    played: heard,
                }
            } else {
                let written = self.written.load(Ordering::SeqCst);
                let played = written.saturating_sub(self.lag);
                self.played.store(played, Ordering::SeqCst);
                sn_media::Position { written, played }
            }
        }

        fn wait_for(&self, _frames: usize, _timeout: Duration) -> bool {
            !self.stopped.load(Ordering::SeqCst)
        }

        fn write(&self, frames: &[i16]) -> Result<usize, sn_media::AudioError> {
            let n = (frames.len() / CHANNELS as usize) as u64;
            self.written.fetch_add(n, Ordering::SeqCst);
            self.counters.store(self.written.load(Ordering::SeqCst), Ordering::SeqCst);
            Ok(n as usize)
        }

        fn underruns(&self) -> u64 {
            0
        }

        fn take_error(&self) -> Option<String> {
            None
        }

        fn drain(&self) {}

        fn state_name(&self) -> &'static str {
            "fake"
        }

        /// The rewind that makes the post-stop position the heard position.
        fn stop(&self) {
            let heard = self.written.load(Ordering::SeqCst).saturating_sub(self.lag);
            self.played.store(heard, Ordering::SeqCst);
            self.written.store(heard, Ordering::SeqCst);
            self.stopped.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn a_lagging_ring_never_bookmarks_past_what_was_heard() {
        let (engine, gate) = FakeEngine::new();
        gate.block_call(2);
        let source = text(5);
        let plan = SpeechPlan::build(&source, 0);
        // A ring a quarter-chunk deep: enough that the heard position sits
        // strictly inside chunk 1 while chunks 0 and 1 are fully written —
        // the shape every real device shows during playback.
        let (sink, counters) = LaggingSink::new(260);
        let cfg = SessionConfig {
            device: "lagging-fake".to_string(),
            channel_depth: 2,
            pinned_opener_chars: Some(crate::plan::FIRST_CHUNK_CHARS),
        };
        let core = SessionCore::start_with_device(&source, 0, engine, cfg, sink).unwrap();
        assert_eq!(gate.wait_blocked(), 2);
        assert!(wait_for(Duration::from_secs(5), || {
            core.progress().chunks_done == 2
        }));

        // The sink model's numbers, read before the pause: two chunks
        // written, only `written − lag` of them heard — part-way into
        // chunk 1, never all of it.
        let written = counters.load(Ordering::SeqCst);
        core.pause();
        gate.release(2);
        assert!(wait_for(Duration::from_secs(5), || {
            core.progress().state == PlayState::Paused
        }));

        // Per-chunk device frames, from the completed chunk's own metric.
        let frontier = core.progress().frontier;
        // THE BUG THIS PINS: the old conversion attributed every written
        // frame to the newest chunk and bookmarked chunk 1's *end* here,
        // ringing up ~300 frames (a fifth of a second) of never-heard audio.
        assert!(
            frontier < plan.chunks[1].end(),
            "frontier {frontier} reached the end of the chunk; the ring's backlog was attributed to it"
        );
        assert!(
            frontier > plan.chunks[1].start,
            "frontier {frontier} shows no progress inside chunk 1"
        );

        // And it is exactly the model's heard position, not approximately it.
        // Chunk 0's audio metric carries its device-frame count; both chunks
        // synthesise identically.
        let metrics = core.join();
        let c0 = &metrics.chunks[0];
        let frames_per_chunk = (c0.audio.as_secs_f64() * f64::from(SAMPLE_RATE)).round() as u64;
        let heard = written - 260;
        let in_chunk1 = heard - frames_per_chunk;
        let expected = plan.chunks[1].start
            + ((in_chunk1 as usize * plan.chunks[1].len) / frames_per_chunk as usize);
        assert_eq!(
            frontier, expected,
            "frontier must be the heard byte, computed from the ring model"
        );
    }

    #[test]
    fn resuming_with_a_dead_device_fails_the_session_instead_of_wedging_it() {
        let (engine, gate) = FakeEngine::new();
        gate.block_call(1);
        let core = SessionCore::start(&text(4), 0, engine, cfg()).unwrap();
        assert_eq!(gate.wait_blocked(), 1);
        assert!(wait_for(Duration::from_secs(5), || {
            core.progress().chunks_done == 1
        }));
        core.pause();
        gate.release(1);
        assert!(wait_for(Duration::from_secs(5), || {
            core.progress().state == PlayState::Paused
        }));

        // The device is gone (unplugged, PipeWire restarted). Resume must
        // surface the error AND land in a terminal state: a session left
        // `Playing` with no threads would show "Speaking" forever, and
        // neither pause nor stop could move it.
        let result = core.resume_with(
            Err(TtsError::Audio(sn_media::AudioError::Alsa(
                "device vanished".to_string(),
            ))),
            SpeechPlan::build(&text(4), core.progress().frontier),
        );
        assert!(result.is_err(), "a dead device must fail the resume");
        assert_eq!(core.progress().state, PlayState::Failed);
        assert!(core.progress().state.is_terminal());
        assert!(core.progress().error.is_some());
    }

    #[test]
    fn metrics_helpers_agree_with_their_parts() {
        let m = Metrics {
            state: PlayState::Finished,
            ttfa: Some(Duration::from_millis(10)),
            frontier: 0,
            errors: 0,
            resume_frontier: 0,
            chunks: vec![
                ChunkMetric {
                    index: 0,
                    chars: 10,
                    beeped: false,
                    synth: Duration::from_millis(100),
                    audio: Duration::from_secs_f64(1.0),
                },
                ChunkMetric {
                    index: 1,
                    chars: 8,
                    beeped: true,
                    synth: Duration::ZERO,
                    audio: Duration::from_secs_f64(crate::beep::BEEP_SECONDS),
                },
            ],
        };
        assert!(
            (m.mean_rtf() - 0.1).abs() < 1e-9,
            "mean RTF is over real chunks only"
        );
        assert_eq!(m.synth_total(), Duration::from_millis(100));
        assert!(
            (m.audio_total().as_secs_f64() - (1.0 + crate::beep::BEEP_SECONDS)).abs() < 0.01
        );
    }
}
