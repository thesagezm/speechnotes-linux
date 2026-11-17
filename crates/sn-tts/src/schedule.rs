//! The chunk-token schedule — the dial between "speech starts fast" and
//! "speech does not run dry".
//!
//! Port of the iOS app's `ChunkSchedule` (SpeechLogic), which replaced a
//! fixed opener character count after the device log showed why a count
//! cannot work:
//!
//! 1. **A character count cannot see the rate of speech.** The same 54
//!    characters were 7.64 s of audio in one session and 3.30 s in another —
//!    an opener sized for one session was far too small (or needily large)
//!    for the next. The gap the schedule exists to prevent is measured in
//!    *seconds*, so the target is seconds of audio and the character count
//!    is derived from a chars-per-audio-second figure the engine measured
//!    for itself.
//! 2. **A schedule that forgets its own success oscillates.** Resetting to
//!    the bottom after any covered session snapped the opener small and the
//!    next session ran short again. So the target only *settles* — to the
//!    measured wait plus slack, never below it.
//!
//! The mechanism: time-to-first-audio is the opener's synthesis time;
//! time-to-second-audio is the second chunk's. If the second chunk arrives
//! after the opener finished *playing*, there is an audible gap. The opener
//! therefore carries a *target in audio seconds* big enough to cover the
//! wait: it grows (×2, floored at the measured wait × headroom) when a
//! session's second buffer was late, and settles (to wait + slack, capped by
//! a fractional shrink) when the cover was comfortable. The character
//! budget the chunker actually takes is the target × the engine's
//! chars-per-audio-second at the live rate, clamped to the engine's ceiling.
//!
//! State is per engine name and lives for the process — the resident engine
//! outlives any one session, which is exactly what lets the *next* session
//! learn from this one.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

/// The opening target for an engine with no measured history, in seconds of
/// speech the opener should carry. iOS's device-log seed: Kokoro's second
/// buffer arrived 7.4 s after its first, so 8 s is the smallest target that
/// covers it.
const INITIAL_TARGET_SECONDS: f64 = 8.0;

/// Supertonic's seed (device log: the second buffer arrived 8.5–13.2 s after
/// the first, so 10 s covers the typical case and the schedule grows past it
/// when the machine is slower still).
const SUPERTONIC_TARGET_SECONDS: f64 = 10.0;

/// The ceiling on the target. Past this a bigger opener cannot be rendered
/// in one go anyway; an engine too slow to benefit from one gets no
/// fast-start rather than a longer silence.
const MAXIMUM_TARGET_SECONDS: f64 = 30.0;

/// The floor. Below this the opener cannot cover anything and shrinking
/// further just guarantees another gap.
const MINIMUM_TARGET_SECONDS: f64 = 3.0;

/// How much of the opener's audio must be LEFT OVER after the wait before
/// the schedule probes back down. Without the slack requirement one
/// covered-by-10 ms session would shrink the opener and the very next
/// session would be short again.
const SHRINK_SLACK_SECONDS: f64 = 2.0;

/// How much of the measured wait the next target must cover when the
/// schedule grows: the new opener gets a margin over the wait that actually
/// happened, so one bad session does not need another.
const GROW_HEADROOM: f64 = 1.2;

/// The fraction of the current target a comfortable cover may give back — a
/// CEILING on the shrink, never the target itself: the measured wait (plus
/// slack) is what the new target settles on, so a shrink can never fall
/// below the wait it must cover.
const SHRINK_FACTOR: f64 = 0.75;

/// Fallback chars-per-audio-second before the engine has measured its own.
const DEFAULT_CHARS_PER_AUDIO_SECOND: f64 = 15.0;

/// Supertonic's fallback (device log: 54 chars → 7.64 s, 157 chars → 13.6 s).
const SUPERTONIC_CHARS_PER_AUDIO_SECOND: f64 = 10.0;

/// Kokoro's fallback (device log: 54 chars → 3.30 s).
const KOKORO_CHARS_PER_AUDIO_SECOND: f64 = 16.0;

/// One engine's schedule state.
#[derive(Debug, Clone)]
pub struct ScheduleState {
    /// The audio-seconds target the next session's opener should meet.
    target_seconds: f64,
    /// The wait from the first buffer to the second, measured by the most
    /// recent session that reported one. `None` = never measured. A session
    /// that ends with no second buffer compares its opener against this.
    last_second_buffer: Option<f64>,
    /// Chars-per-audio-second this engine actually spoke, measured from the
    /// last session's opener. `None` until a session reports real audio.
    measured_chars_per_second: Option<f64>,
    /// The speech rate the measurement was taken at — scaled by
    /// `rate / measured_at_rate` when the rate changes.
    measured_at_rate: f64,
    /// Chars-per-audio-second until the engine has measured its own, seeded
    /// per engine from the device logs.
    fallback_chars_per_second: f64,
}

impl ScheduleState {
    /// A fresh schedule for `engine`, seeded from the measured waits in the
    /// iOS device logs.
    pub fn for_engine(engine: &str) -> Self {
        let (target, chars) = match engine {
            "supertonic" => (SUPERTONIC_TARGET_SECONDS, SUPERTONIC_CHARS_PER_AUDIO_SECOND),
            "kokoro" => (INITIAL_TARGET_SECONDS, KOKORO_CHARS_PER_AUDIO_SECOND),
            _ => (INITIAL_TARGET_SECONDS, DEFAULT_CHARS_PER_AUDIO_SECOND),
        };
        Self {
            target_seconds: target,
            last_second_buffer: None,
            measured_chars_per_second: None,
            measured_at_rate: 1.0,
            fallback_chars_per_second: chars,
        }
    }

    fn clamp_target(value: f64) -> f64 {
        value.clamp(MINIMUM_TARGET_SECONDS, MAXIMUM_TARGET_SECONDS)
    }

    /// The current target, inside the legal band.
    pub fn current_target_seconds(&self) -> f64 {
        Self::clamp_target(self.target_seconds)
    }

    /// Record one session's outcome: whether the second buffer was late (the
    /// audible gap), what the opener's first chunk carried (audio + chars —
    /// the chars-per-audio-second measurement), and the wait from the first
    /// buffer to the second (`None` when no second buffer arrived — a
    /// single-chunk text, or the session ended early).
    pub fn record(
        &mut self,
        second_buffer_was_late: Option<bool>,
        opener_audio: Option<Duration>,
        opener_chars: usize,
        second_buffer_wait: Option<Duration>,
        rate: f64,
    ) {
        let audio = opener_audio.map(|d| d.as_secs_f64()).unwrap_or(0.0);
        if let Some(wait) = second_buffer_wait {
            self.last_second_buffer = Some(wait.as_secs_f64());
        }
        // Learn this engine's chars-per-audio-second. A session that
        // produced no audio (a beep, a failure) teaches nothing.
        if audio > 0.0 && opener_chars > 0 {
            self.measured_chars_per_second = Some(opener_chars as f64 / audio);
            self.measured_at_rate = if rate > 0.0 { rate } else { 1.0 };
        }
        let Some(wait) = second_buffer_wait.map(|d| d.as_secs_f64()) else {
            // No second buffer: the session taught nothing about the wait.
            // But the opener's own audio still says something — if it was
            // smaller than the wait this engine is KNOWN to take, the opener
            // is too small and grows.
            if let Some(remembered) = self.last_second_buffer {
                if audio > 0.0 && audio < remembered {
                    self.target_seconds = Self::clamp_target(
                        (self.target_seconds * 2.0).max(remembered * GROW_HEADROOM),
                    );
                }
            }
            return;
        };
        if second_buffer_was_late == Some(true) {
            // The gap happened: grow until the opener would have covered it.
            // Both steps are ABOVE the wait, so the next session cannot be
            // short for the same reason.
            self.target_seconds =
                Self::clamp_target((self.target_seconds * 2.0).max(wait * GROW_HEADROOM));
            return;
        }
        // Covered. Hold the target unless the cover was comfortable — then
        // settle it just above the wait that actually happened, never below
        // it. `wait + slack` and not a fraction of the old target: a
        // fractional shrink of a target that merely happened to cover can
        // land BELOW the wait it has to cover, and the next session is short
        // again — the oscillation this schedule exists to end.
        let slack = audio - wait;
        if slack >= SHRINK_SLACK_SECONDS {
            let settled = (wait + SHRINK_SLACK_SECONDS).max(self.target_seconds * SHRINK_FACTOR);
            self.target_seconds = Self::clamp_target(settled.min(self.target_seconds));
        }
    }

    /// The state one line, for the log: the opener's outcome and the budget
    /// it leaves behind. The schedule is otherwise invisible — a gate that
    /// wants to know whether the opener learned has nowhere to look.
    ///
    /// Test-only: no production caller (the session tail line does not use
    /// it); kept for the schedule tests that assert the budget arithmetic.
    #[cfg(test)]
    fn summary(&self, rate: f64, batch_ceiling: usize) -> String {
        format!(
            "target={:.1}s chars/s={} budget={} chars",
            self.current_target_seconds(),
            self.measured_chars_per_second
                .map(|c| format!("{c:.1}"))
                .unwrap_or_else(|| "unmeasured".into()),
            self.opener_character_budget(rate, batch_ceiling),
        )
    }

    /// The character budget for the opener: the target in seconds × this
    /// engine's chars-per-audio-second at the live rate, clamped to the
    /// engine's own hard ceiling (an opener above it would be split by the
    /// chunker — the opposite of a fast opener).
    pub fn opener_character_budget(&self, rate: f64, batch_ceiling: usize) -> usize {
        let effective_rate = if rate > 0.0 { rate } else { 1.0 };
        let chars_per_second = match self.measured_chars_per_second {
            Some(measured) if measured > 0.0 => {
                // The same characters take proportionally less audio when the
                // voice is faster, so MORE characters fit the same target.
                measured * (effective_rate / self.measured_at_rate)
            }
            _ => self.fallback_chars_per_second * effective_rate,
        };
        let raw = self.current_target_seconds() * chars_per_second;
        (raw.ceil().max(1.0) as usize).min(batch_ceiling.max(1))
    }
}

/// The process-wide schedules, one per engine name. The resident engine
/// outlives any one session; the *next* session learns from this one.
static SCHEDULES: LazyLock<Mutex<HashMap<&'static str, ScheduleState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn with_state<R>(engine: &'static str, f: impl FnOnce(&mut ScheduleState) -> R) -> R {
    let mut map = SCHEDULES.lock().unwrap_or_else(|e| e.into_inner());
    f(map.entry(engine).or_insert_with(|| ScheduleState::for_engine(engine)))
}

/// The opener's character budget for `engine` at `rate`, clamped to
/// `batch_ceiling`. Called at session start, before the plan is built.
pub fn opener_budget(engine: &'static str, rate: f64, batch_ceiling: usize) -> usize {
    with_state(engine, |s| s.opener_character_budget(rate, batch_ceiling))
}

/// Record a session's opener outcome (see [`ScheduleState::record`]). Called
/// by the audio thread once the first two chunks' fates are known.
pub fn record(
    engine: &'static str,
    second_buffer_was_late: Option<bool>,
    opener_audio: Option<Duration>,
    opener_chars: usize,
    second_buffer_wait: Option<Duration>,
    rate: f64,
) {
    with_state(engine, |s| {
        s.record(
            second_buffer_was_late,
            opener_audio,
            opener_chars,
            second_buffer_wait,
            rate,
        )
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(engine: &str) -> ScheduleState {
        ScheduleState::for_engine(engine)
    }

    #[test]
    fn the_budget_starts_at_the_seeded_target() {
        // Supertonic: 10 s × 10 chars/s = 100 characters — the neighbourhood
        // of the fixed 110-char opener this schedule replaces.
        let s = state("supertonic");
        assert_eq!(s.opener_character_budget(1.0, 300), 100);
        // Kokoro speaks faster: the same 8 s target needs more characters.
        assert_eq!(state("kokoro").opener_character_budget(1.0, 300), 128);
    }

    #[test]
    fn a_faster_rate_needs_more_characters_for_the_same_target() {
        let s = state("supertonic");
        let at_2x = s.opener_character_budget(2.0, 300);
        assert!(at_2x > 100, "rate 2.0 must widen the budget, got {at_2x}");
        // And the ceiling holds.
        assert!(at_2x <= 300);
    }

    #[test]
    fn a_late_second_buffer_grows_the_target() {
        let mut s = state("kokoro");
        let before = s.current_target_seconds();
        // 6 s of opener audio, the second buffer 9 s later: a 3 s gap.
        s.record(
            Some(true),
            Some(Duration::from_secs_f64(6.0)),
            100,
            Some(Duration::from_secs_f64(9.0)),
            1.0,
        );
        // The grow rule takes the coarser of doubling and the measured wait
        // with headroom — max(2 × 8, 9 × 1.2) = 16 — so the next session
        // cannot be short for the same reason either way.
        assert_eq!(s.current_target_seconds(), 16.0);
        assert!(s.current_target_seconds() > before);
        // And the measurement landed: 100 chars / 6 s ≈ 16.7 chars/s, so the
        // 16 s target asks ≈ 267 characters — inside the 300 ceiling.
        let budget = s.opener_character_budget(1.0, 300);
        assert!((budget as f64 - 267.0).abs() < 3.0, "budget {budget}");
        assert!(budget <= 300);
    }

    #[test]
    fn a_comfortable_cover_settles_just_above_the_wait() {
        let mut s = state("kokoro");
        s.record(
            Some(false),
            Some(Duration::from_secs_f64(12.0)),
            190,
            Some(Duration::from_secs_f64(3.0)),
            1.0,
        );
        // Covered with 9 s of slack: the target settles at wait + slack
        // (5.0), bounded above by a fractional shrink of the old target
        // (0.75 × 8 = 6.0) — max picks 6.0, and it can only go down from 8.
        assert_eq!(s.current_target_seconds(), 6.0);
    }

    #[test]
    fn a_shrink_never_falls_below_the_wait_it_must_cover() {
        let mut s = state("kokoro");
        s.target_seconds = 30.0;
        // Covered with 3 s of slack (12 s opener, 9 s wait): the shrink
        // ceiling (0.75 × 30 = 22.5) is far above wait + slack (11.0), so
        // the settle rule picks 22.5 — still comfortably above the 9 s wait
        // it has to cover.
        s.record(
            Some(false),
            Some(Duration::from_secs_f64(12.0)),
            190,
            Some(Duration::from_secs_f64(9.0)),
            1.0,
        );
        assert_eq!(s.current_target_seconds(), 22.5);
    }

    #[test]
    fn an_uncomfortable_cover_holds_the_target() {
        let mut s = state("kokoro");
        s.target_seconds = 30.0;
        // 10 s opener, 9 s wait: covered, but only 1 s of slack — below the
        // shrink threshold. One covered-by-a-whisker session must not shrink
        // the opener; the very next session would be short again.
        s.record(
            Some(false),
            Some(Duration::from_secs_f64(10.0)),
            160,
            Some(Duration::from_secs_f64(9.0)),
            1.0,
        );
        assert_eq!(s.current_target_seconds(), 30.0);
    }

    #[test]
    fn no_second_buffer_still_grows_a_short_opener() {
        let mut s = state("kokoro");
        s.last_second_buffer = Some(7.0);
        // The session ended after a 3 s opener with no second buffer; the
        // engine is known to wait 7 s. The opener was too small: the grow
        // rule takes max(2 × 8, 7 × 1.2) = 16.
        s.record(None, Some(Duration::from_secs_f64(3.0)), 50, None, 1.0);
        assert_eq!(s.current_target_seconds(), 16.0);
    }

    #[test]
    fn a_session_that_teaches_nothing_changes_nothing() {
        let mut s = state("kokoro");
        let before = s.current_target_seconds();
        // A beep (no audio) as the opener: no chars-per-second learning, no
        // target move.
        s.record(Some(false), Some(Duration::from_secs_f64(0.0)), 0, None, 1.0);
        assert_eq!(s.current_target_seconds(), before);
        assert!(s.measured_chars_per_second.is_none());
    }

    #[test]
    fn the_measurement_follows_the_rate() {
        let mut s = state("supertonic");
        // 100 chars in 10 s at rate 1.0 → 10 chars/s.
        s.record(
            Some(false),
            Some(Duration::from_secs_f64(10.0)),
            100,
            Some(Duration::from_secs_f64(2.0)),
            1.0,
        );
        // At rate 2.0 the same audio takes half the time, so twice the
        // characters fit the same 8-ish-second target: 10 × (2.0/1.0) = 20
        // chars/s.
        let target = s.current_target_seconds();
        assert!((s.opener_character_budget(2.0, 300) as f64) >= target * 20.0 - 1.0);
    }
}
