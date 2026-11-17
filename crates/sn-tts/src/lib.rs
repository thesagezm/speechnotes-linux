//! Text-to-speech: the engine tier, the playback pipeline and the bookkeeping
//! that turns a note's text into heard speech.
//!
//! The layering, from text to sound:
//!
//! 1. [`plan::SpeechPlan`] — the note's text through the offset-preserving
//!    sanitiser and the sentence chunker, so every chunk carries the byte
//!    offsets it came from. Byte offsets into the plan are byte offsets into
//!    the note the user sees, which is what makes read-along and bookmarks
//!    possible at all.
//! 2. [`engine::Engine`] — an implementation synthesises one chunk of text to
//!    PCM. Batch 5 ships the [`espeak::EspeakEngine`] (the CLI, since the box
//!    has no libespeak dev headers); Batch 6 adds the neural engines behind
//!    the same trait.
//! 3. [`session::SessionCore`] — the pipeline: a synth thread that runs one
//!    chunk ahead of an audio thread feeding the sn-media sink. It owns the
//!    played frontier (a byte offset, updated from *played* frames, never
//!    written ones), the pause/stop/resume state machine, the soft-beep
//!    skip-on-failure path, and the TTFA / per-chunk-RTF metrics.
//!
//! Two invariants the rest of the family depends on, both inherited from the
//! iOS bugs this rewrite exists to bury:
//!
//! * **The frontier is a field of the playback state**, and every bookmark is
//!   written *from* it. Nothing reads a bookmark back into the pipeline.
//! * **Completion is a signal, not a heuristic.** `Finished` is set only when
//!   the last chunk has actually been played; nothing infers it from progress.
//!
//! Failure philosophy (the family rule): one attempt per chunk, then a soft
//! beep, then the next sentence. A second attempt fails the same way, and the
//! wait for it is silence the listener pays for.

pub mod audiobook;
pub mod beep;
pub mod ortrt;
pub mod kokoro;
pub mod manager;
pub mod nano_edge;
pub mod phonon;
pub mod supertonic;
pub mod phonemes;
pub mod engine;
pub mod espeak;
pub mod plan;
pub mod schedule;
pub mod session;

pub use engine::{Engine, Synthesis, Voice};
pub use espeak::EspeakEngine;
pub use plan::{SpeechPlan, MAX_CHUNK_CHARS};
pub use session::{
    ChunkMetric, Metrics, PlayState, Progress, SessionConfig, SessionCore,
};

/// Everything that can go wrong between text and PCM.
#[derive(Debug, thiserror::Error)]
pub enum TtsError {
    #[error("nothing speakable was supplied")]
    Empty,
    #[error("the {engine} engine is not available: {detail}")]
    Unavailable { engine: &'static str, detail: String },
    #[error("model load failed: {0}")]
    ModelLoad(String),
    #[error("synthesis failed: {0}")]
    Synthesis(String),
    #[error("{0}")]
    State(String),
    #[error("synthesis timed out after {0:?}")]
    Timeout(std::time::Duration),
    #[error("audio output failed: {0}")]
    Audio(#[from] sn_media::AudioError),
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
}
