//! The speech plan: a note's text as the engine will consume it, with byte
//! offsets intact.
//!
//! The pipeline is `clean()` → `plan_chunks()` → speakable filter, and every
//! stage preserves byte offsets:
//!
//! * [`sn_format::sanitizer::clean`] emits exactly one byte per input byte
//!   (removed markup becomes spaces), so a byte offset in the cleaned text is
//!   the same offset in the note the user sees;
//! * [`sn_format::chunker::plan_chunks`] tiles the text losslessly and takes
//!   the resume offset directly.
//!
//! That chain is the whole reason read-along and bookmarks work: the frontier
//! the pipeline reports, the sentence the UI highlights, and the bookmark the
//! store saves are all *the same number*. Any filtering that happens inside
//! the chunker instead of here would break the tiling and silently corrupt
//! resume — the exact bug class this rewrite exists to remove.

use sn_format::chunker::{self, Chunk};
use sn_format::sanitizer;

/// Maximum characters per synthesis call.
///
/// Small enough that one chunk is one sentence-ish unit (read-along and
/// bookmark granularity), large enough that espeak's prosody across a
/// sentence survives. The chunker still ends chunks at sentence boundaries
/// first; this only caps a run-on sentence.
///
/// Equal to `chunker::MAX_ENGINE_CHARS` on purpose: that one is the ceiling no
/// engine accepts a chunk above (Supertonic beeps and skips past 300), and a
/// pipeline asking for anything larger would be relying on the chunker's
/// `hard_cap` headroom to save it.
pub const MAX_CHUNK_CHARS: usize = chunker::MAX_ENGINE_CHARS;

/// The opener's cap, in characters.
///
/// Time to first audio is the first chunk's synthesis time, and a
/// 300-character opener is 15–30 audio-seconds on the slow engine — 7–13 s
/// of silence before the first word. The iOS app's measured rule
/// (`ChunkSchedule` + `firstMaxChars`): a short opener that plays almost
/// immediately, full-size chunks after it. ~110 characters is ~10
/// audio-seconds on Supertonic, ~7 on Kokoro — a first word in 3–5 s instead
/// of 7–13.
pub const FIRST_CHUNK_CHARS: usize = 110;

/// A plan of what to speak, in order.
#[derive(Debug, Clone)]
pub struct SpeechPlan {
    /// The chunks worth sending to an engine, in order, renumbered from zero
    /// (`plan_chunks` already renumbers; the speakable filter preserves the
    /// surviving indices' order, and `index` is rebuilt to match).
    pub chunks: Vec<Chunk>,
}

impl SpeechPlan {
/// Plan from a note's raw text, resuming at `resume_byte`, with the opener
/// capped at [`FIRST_CHUNK_CHARS`].
///
/// Chunks that end at or before the resume offset are dropped, so a
/// resumed plan's first chunk is the one the listener was inside.
pub fn build(text: &str, resume_byte: usize) -> SpeechPlan {
    Self::build_with_opener(text, resume_byte, FIRST_CHUNK_CHARS)
}

/// [`build`] with the opener's character budget decided by the caller —
/// [`crate::schedule`] sizes it in audio seconds from what the engine
/// actually measured; the fixed cap is the no-history fallback.
///
/// A budget of `usize::MAX` means "no opener treatment": the chunker packs
/// the first chunk to the engine ceiling. The tests use it to pin the
/// chunker's own behaviour independently of the schedule.
pub fn build_with_opener(text: &str, resume_byte: usize, opener_chars: usize) -> SpeechPlan {
    let cleaned = sanitizer::clean(text);
    let planned = chunker::plan_chunks(&cleaned, MAX_CHUNK_CHARS, resume_byte);
    let planned = split_opener(planned, &cleaned, opener_chars);
    let chunks: Vec<Chunk> = planned
        .into_iter()
        .filter(|c| sanitizer::is_speakable(&c.text))
        .enumerate()
        .map(|(index, mut c)| {
            c.index = index;
            c
        })
        .collect();
    SpeechPlan { chunks }
}

    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    pub fn len(&self) -> usize {
        self.chunks.len()
    }

    /// The byte offset playback reaches when everything has been spoken: the
    /// last chunk's end. Zero for an empty plan.
    pub fn end_byte(&self) -> usize {
        self.chunks.last().map(Chunk::end).unwrap_or(0)
    }

    /// The chunk containing `byte`, if any.
    ///
    /// Binary search: this runs on every read-along tick, and a linear scan
    /// over a 200k-character plan would be the quadratic behaviour this
    /// rewrite exists to remove.
    pub fn chunk_at(&self, byte: usize) -> Option<&Chunk> {
        let i = self
            .chunks
            .partition_point(|c| c.end() <= byte)
            .min(self.chunks.len().saturating_sub(1));
        self.chunks.get(i).filter(|c| byte < c.end())
    }
}

/// Shape the plan's opener to `opener_chars`, in whichever direction it is
/// off.
///
/// **Shrink** — the first chunk is over budget (a long sentence, or the
/// remainder one a resume cut out of a long sentence), so time to first
/// audio would be that whole span's synthesis. It is re-planned at the
/// smaller ceiling over **its own span**: after a resume that span starts
/// mid-text, and re-planning from byte 0 would resurrect already-spoken
/// text. Its pieces end exactly where the chunk ended, so every later chunk
/// is untouched and the tiling stays lossless. No boundary to split at (the
/// re-plan returns one piece) keeps the chunk whole — the chunker's
/// `hard_cap` headroom cuts it for the engine, and a plan cannot invent a
/// boundary that is not there.
///
/// The LOUD fix (2026-10-31): the
/// original carried a second, **Grow** arm here — when the first chunk was
/// under budget it merged whole following chunks into the opener while they
/// fit (added Oct-27 18:57, contradicting this file's own pre-grow original
/// tests). It is REMOVED, deliberately and loudly, because:
///
/// 1. **It is the user-facing bug the round-4 brief names.** A 3-sentence
///    note planned as one chunk; a resume produced 2 chunks where 3 are
///    wanted (selftest `plan_check` "resume produced 2 chunks, wanted 3";
///    sn-tts's `resume_drops_only_what_came_before` and
///    `chunk_at_finds_the_owner_of_a_byte` panicked on `chunks[1]`;
///    session's pause/resume shape test failed for the same reason).
/// 2. **It degrades read-along and bookmark granularity.** The session's
///    read-along highlight and the bookmark frontier are chunk-grained: a
///    merged 110-char opener highlights (and bookmarks) three sentences as
///    one block. The chunker's contract — one sentence per chunk, the
///    property this file's doc comment calls the reason read-along works —
///    is the behavior every original test pins.
/// 3. **The audible gap it tried to close is covered elsewhere.** The
///    session's adaptive lookahead pacing (schedule.rs sizing, engine TTFA
///    measured in round 3: supertonic 1.26 s warm) keeps the
///    second buffer ahead of playback without destroying chunk structure.
///    If a scheduled opener ever needs more audio than one sentence, the
///    fix belongs in `plan_chunks` (pack the first chunk to the budget with
///    real sentence boundaries), not in merging finished chunks after the
///    fact.
fn split_opener(chunks: Vec<Chunk>, source: &str, opener_chars: usize) -> Vec<Chunk> {
    let Some(first) = chunks.first() else {
        return chunks;
    };
    let first_len = first.text.chars().count();
    if first_len <= opener_chars {
        return chunks;
    }
    let region = first.start..first.end();
    // (The original's Grow arm read this field through a method call; the
    // Oct-22 chunker exposes `start` as a field.)
    let opener = chunker::plan_chunks(&source[region.clone()], opener_chars, 0);
    if opener.len() < 2 {
        return chunks;
    }
    let mut out: Vec<Chunk> = opener
        .into_iter()
        .map(|mut c| {
            c.start += region.start;
            c
        })
        .collect();
    out.extend(chunks.into_iter().skip(1));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_survive_sanitising_so_bookmarks_stay_valid() {
        let text = "# Heading\n\nA sentence with **bold** and a [link](http://x.example).\n\n```rust\nlet code = 1;\n```\n\nTail sentence.";
        let plan = SpeechPlan::build(text, 0);
        assert!(!plan.is_empty());
        let cleaned = sanitizer::clean(text);
        for c in &plan.chunks {
            // Every chunk's text must be the source's own bytes: offsets are
            // only meaningful if the plan never rewrites text in place.
            let slice = cleaned.get(c.start..c.end()).unwrap();
            assert_eq!(slice, c.text, "chunk {} diverges from the source", c.index);
        }
        // The fence content must not be spoken.
        assert!(plan.chunks.iter().all(|c| !c.text.contains("let code")));
        // ...and the tail sentence must still be there.
        assert!(plan.chunks.iter().any(|c| c.text.contains("Tail sentence")));
    }

    #[test]
    fn resume_drops_only_what_came_before() {
        let text = "First sentence here. Second sentence follows. Third and final one.";
        let plan = SpeechPlan::build(text, 0);
        let mid = plan.chunks[1].start;
        let resumed = SpeechPlan::build(text, mid);
        assert_eq!(resumed.chunks[0].start, mid, "resume starts mid-text");
        assert_eq!(resumed.len(), plan.len() - 1);
        // A resume from the very end is an empty plan, not a panic.
        assert!(SpeechPlan::build(text, text.len()).is_empty());
    }

    /// The LOUD split_opener fix: pins the corrected
    /// behavior the user asked for — short sentences stay their own chunks,
    /// fresh and on resume. The original's Grow arm merged whole following
    /// chunks into the opener while they fit the budget, which turned this
    /// 3-sentence note into ONE chunk ("resume produced 2 chunks, wanted 3"
    /// was the selftest's face of it). One sentence per chunk is the
    /// read-along and bookmark granularity; see split_opener's doc comment
    /// for the full rationale.
    #[test]
    fn short_sentences_stay_their_own_chunks_fresh_and_on_resume() {
        let text = "First sentence here. Second sentence follows. Third and final one.";
        let plan = SpeechPlan::build(text, 0);
        assert_eq!(plan.len(), 3, "{:?}", plan.chunks.iter().map(|c| c.text.clone()).collect::<Vec<_>>());
        // Each chunk is exactly its own sentence, in order.
        assert!(plan.chunks[1].text.starts_with("Second sentence follows."));
        let resumed = SpeechPlan::build(text, plan.chunks[1].start);
        assert_eq!(resumed.len(), 2, "the resume speaks the two remaining sentences as two chunks");
        assert!(resumed.chunks[0].text.starts_with("Second sentence"));
    }

    #[test]
    fn unspeakable_chunks_are_filtered_at_the_boundary() {
        // A run of punctuation between two sentences is silence at best and a
        // model error at worst; it must not reach an engine.
        let text = "Before. ... !!! — After.";
        let plan = SpeechPlan::build(text, 0);
        assert_eq!(plan.len(), 2, "{:?}", plan.chunks.iter().map(|c| c.text.clone()).collect::<Vec<_>>());
    }

    #[test]
    fn chunk_at_finds_the_owner_of_a_byte() {
        let text = "One two three. Four five six. Seven eight nine.";
        let plan = SpeechPlan::build(text, 0);
        let c0 = &plan.chunks[0];
        let c1 = &plan.chunks[1];
        assert_eq!(plan.chunk_at(c0.start).map(|c| c.index), Some(0));
        assert_eq!(plan.chunk_at(c1.start).map(|c| c.index), Some(1));
        assert_eq!(plan.chunk_at(c0.end() - 1).map(|c| c.index), Some(0));
        // A boundary byte belongs to the *following* chunk (each chunk's end
        // is exclusive to itself), and past the end there is nothing.
        assert_eq!(plan.chunk_at(plan.end_byte()).map(|c| c.index), None);
    }

    #[test]
    fn empty_and_unspeakable_text_give_empty_plans() {
        assert!(SpeechPlan::build("", 0).is_empty());
        assert!(SpeechPlan::build("```\nonly code\n```", 0).is_empty());
        assert!(SpeechPlan::build("---\n***\n...", 0).is_empty());
    }

    #[test]
    fn the_opener_is_short_and_the_plan_still_tiles() {
        // Four full-length sentences: without the opener cap the first chunk
        // packs ~300 characters, which is 7–13 s of silence before the first
        // word on the neural engines. The cap cuts the opener; the plan must
        // still tile the cleaned text exactly.
        let text = "One two three four five six seven eight nine ten eleven twelve. \
                    The second sentence rides along to fill the first chunk past the cap. \
                    A third sentence keeps the plan long enough to matter. \
                    A fourth sentence closes the text out.";
        let cleaned = sanitizer::clean(text);
        let plan = SpeechPlan::build(text, 0);
        assert!(plan.len() >= 2);
        let opener_chars = plan.chunks[0].text.chars().count();
        assert!(
            opener_chars <= FIRST_CHUNK_CHARS,
            "the opener is {opener_chars} chars, the cap is {FIRST_CHUNK_CHARS}"
        );
        // The tiles still cover the source: every chunk's text is the
        // source's own bytes, and anything between two chunks is silence the
        // speakable filter dropped — never a lost sentence.
        let mut cursor = 0usize;
        for c in &plan.chunks {
            assert!(c.start >= cursor, "chunks went backwards at {}", c.start);
            if c.start > cursor {
                let gap = &cleaned[cursor..c.start];
                assert!(
                    !sanitizer::is_speakable(gap),
                    "speakable text was skipped at {cursor}: {gap:?}"
                );
            }
            assert_eq!(
                cleaned.get(c.start..c.end()).unwrap(),
                c.text,
                "chunk {} diverges from the source",
                c.index
            );
            cursor = c.end();
        }
        // The whole text is still spoken, in order.
        let spoken: String = plan.chunks.iter().map(|c| c.text.as_str()).collect();
        assert!(spoken.contains("A fourth sentence closes"));
    }

    /// No chunk may exceed the engine ceiling. This is the pipeline-level
    /// statement of the bug the chunker's `hard_cap` fixes: Supertonic turns
    /// a chunk over 300 characters into a beep and a skipped phrase, which
    /// means a phrase missing from a book — not a degraded one. A token with
    /// no break point at all is the shape that used to reach the engine whole.
    ///
    /// A 600-character chemical name, not a URL: the sanitizer drops bare URLs
    /// on purpose (a read-aloud URL is silence anyway), so a URL never reaches
    /// this path. The name is the real shape — a compound identifier in a
    /// textbook has no whitespace and no break point.
    #[test]
    fn no_chunk_exceeds_the_engine_ceiling() {
        let text = format!(
            "The compound {} reacts with water. The mixture should be stirred.",
            "n".repeat(600)
        );
        let plan = SpeechPlan::build(&text, 0);
        assert!(!plan.is_empty());
        for c in &plan.chunks {
            assert!(
                c.text.chars().count() <= MAX_CHUNK_CHARS,
                "chunk {} is {} chars, which no engine will speak",
                c.index,
                c.text.chars().count()
            );
        }
        // The token is still spoken, in pieces: nothing was dropped, and the
        // sentences around it survived.
        let spoken: String = plan.chunks.iter().map(|c| c.text.as_str()).collect();
        assert!(spoken.contains("The compound"), "the lead-in went missing");
        assert!(
            spoken.contains("reacts with water"),
            "the tail went missing: {spoken:?}"
        );
    }
}
