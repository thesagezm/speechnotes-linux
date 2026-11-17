//! The Pomodoro engine — pure data and date math, no widgets. This is the
//! port of `PomodoroEngine.swift`; the unit tests in the parent module pin
//! it. Split out of `pomodoro.rs` (Round 4, pure code-motion).

use serde::{Deserialize, Serialize};

use chrono::TimeZone;

// ===========================================================================
// The engine — pure data and date math, no widgets. Everything below the
// `Store` section is the port of PomodoroEngine.swift, semantics for
// semantics; the unit tests at the foot of this file pin them.
// ===========================================================================

/// A wall-clock instant, in Unix seconds. Every boundary in the engine is
/// one of these.
pub type Unix = i64;

/// The three timer shapes the card offers.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    Structured,
    Flowmodoro,
    Classic,
}

/// One segment of a running timer.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    Prep,
    Focus,
    Rest,
    Done,
}

impl Phase {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Phase::Prep => "Preparing",
            Phase::Focus => "Focus",
            Phase::Rest => "Break",
            Phase::Done => "Done",
        }
    }

    /// The card and banner colour for a phase: prep orange, focus green,
    /// rest blue — one source so "break is blue" everywhere.
    pub(crate) fn css(self) -> &'static str {
        match self {
            Phase::Prep => "prep",
            Phase::Focus => "focus",
            Phase::Rest => "rest",
            Phase::Done => "done",
        }
    }
}

/// A full description of a timer run. The running session snapshots its
/// config at start, so editing preferences never bends a live timer.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub mode: Mode,
    /// Mandatory preparation window before the first focus phase, seconds.
    /// Clamped 120..=300 (2–5 min) by the editor; every mode runs it, and
    /// it is not skippable.
    pub prep_seconds: i64,
    // Structured ------------------------------------------------------------
    /// true → the window is a duration from start; false → a wall-clock
    /// finish time (hour/minute below, next occurrence).
    pub structured_by_duration: bool,
    pub structured_duration_seconds: i64,
    pub structured_finish_hour: i64,
    pub structured_finish_minute: i64,
    pub structured_break_count: i64,
    pub structured_break_seconds: i64,
    // Classic ---------------------------------------------------------------
    /// Focus block lengths in minutes; the count of blocks IS the count.
    pub classic_focus_minutes: Vec<i64>,
    /// Break lengths in minutes; a break follows each block while the array
    /// has entries (breaks.len() <= blocks keeps them strictly in between).
    pub classic_break_minutes: Vec<i64>,
    /// The Linux half of the phase announcement: a soft two-note chime
    /// beside the banner. Not an iOS key — the phone speaks instead.
    pub chime: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: Mode::Classic,
            prep_seconds: 180,
            structured_by_duration: true,
            structured_duration_seconds: 3 * 3600,
            structured_finish_hour: 17,
            structured_finish_minute: 0,
            structured_break_count: 3,
            structured_break_seconds: 5 * 60,
            classic_focus_minutes: vec![25, 25, 25, 25],
            classic_break_minutes: vec![5, 5, 5],
            chime: true,
        }
    }
}

impl Config {
    /// The absolute end of a structured window that starts at `start` —
    /// either start + duration, or the next occurrence of the finish
    /// wall-clock time (today if still ahead, otherwise tomorrow).
    fn structured_window_end(&self, start: Unix) -> Option<Unix> {
        if self.structured_by_duration {
            return Some(start + self.structured_duration_seconds);
        }
        use chrono::Local;
        let start_local = Local.timestamp_opt(start, 0).single()?;
        let today = start_local.date_naive();
        let mut candidate = today
            .and_hms_opt(
                self.structured_finish_hour.clamp(0, 23) as u32,
                self.structured_finish_minute.clamp(0, 59) as u32,
                0,
            )?
            .and_local_timezone(Local)
            .single()?;
        // `after` semantics — the next occurrence strictly after start.
        if candidate.timestamp() <= start {
            candidate += chrono::Duration::days(1);
        }
        Some(candidate.timestamp())
    }

    /// Focus seconds per structured segment for a window of `window`
    /// seconds, or `None` when the window cannot hold prep + the breaks +
    /// at least one focus minute per segment. Residual seconds land in the
    /// final segment (the timeline builder ends it exactly at the window
    /// end).
    fn structured_segment_seconds(&self, window: i64) -> Option<i64> {
        let fixed = self.prep_seconds + self.structured_break_count * self.structured_break_seconds;
        let segments = self.structured_break_count + 1;
        let focus = window - fixed;
        if focus < segments * 60 {
            return None;
        }
        Some(focus / segments)
    }

    /// A human summary of the structured plan ("≈44 min focus × 4 + 5 min
    /// break × 3 + 3 min prep, in 3 h.") — the config sheet's footer and
    /// its start-button gate in one.
    pub(crate) fn structured_summary(&self, now: Unix) -> Option<String> {
        let end = self.structured_window_end(now)?;
        let segment = self.structured_segment_seconds(end - now)?;
        let segments = self.structured_break_count + 1;
        let ends_text = if self.structured_by_duration {
            format!("in {}", duration_text(self.structured_duration_seconds))
        } else {
            let local = chrono::Local.timestamp_opt(end, 0).single()?;
            format!("at {}:{}", local.format("%H"), local.format("%M"))
        };
        let mut parts = vec![format!("≈{} focus × {}", minutes_text(segment), segments)];
        if self.structured_break_count > 0 {
            parts.push(format!(
                "{} min break × {}",
                self.structured_break_seconds / 60,
                self.structured_break_count
            ));
        }
        parts.push(format!("{} min prep", self.prep_seconds / 60));
        Some(format!("{}, {ends_text}.", parts.join(" + ")))
    }

    /// Can this config start? Flowmodoro always; classic needs one block;
    /// structured needs a window that fits.
    pub(crate) fn can_start(&self, now: Unix) -> bool {
        match self.mode {
            Mode::Flowmodoro => true,
            Mode::Classic => !self.classic_focus_minutes.is_empty(),
            Mode::Structured => self.structured_summary(now).is_some(),
        }
    }
}

/// A scheduled step. `end` is absolute for structured AND classic (classic's
/// chained durations are resolved to instants at start, and pauses slide
/// every not-yet-reached end) — `None` only for the open-ended flowmodoro
/// focus, which lives outside the timeline entirely.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub phase: Phase,
    pub end: Option<Unix>,
}

/// One phase that just completed, surfaced so the store can fold focus
/// seconds into history and raise banners.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Event {
    pub completed_phase: Phase,
    /// How long the completed phase ran, seconds. Focus events feed the day
    /// log; partial focus (stop/skip mid-block) is included.
    pub seconds: f64,
}

/// Focus seconds that earn one break minute.
const FLOW_UNIT_FOCUS: f64 = 300.0;
/// Focus seconds before a break may be claimed at all.
const FLOW_CLAIM_THRESHOLD: f64 = 180.0;

/// The live timer, as a plain serde value. All time math is anchored to
/// wall-clock instants, never to tick counts, so the session survives the
/// app being closed: on relaunch the store feeds the current time to
/// [`Session::advance_if_due`] and every phase that elapsed while away is
/// reconciled (and logged) from the timeline alone.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Session {
    pub config: Config,
    pub started_at: Unix,
    /// Current phase; `Done` once the timeline (or a stop) has run out.
    pub phase: Phase,
    /// Start of the current phase (`None` when done).
    pub phase_start: Option<Unix>,
    /// End of the current phase — `None` for the open-ended flowmodoro
    /// focus and for `Done`.
    pub phase_end: Option<Unix>,
    /// Structured/classic: the absolute timeline, with `step_index` pointing
    /// at the current step. Flowmodoro keeps this empty.
    pub steps: Vec<Step>,
    pub step_index: usize,
    /// Non-`None` while paused: the instant the freeze began. Every boundary
    /// slides by the paused span on resume.
    pub paused_at: Option<Unix>,
    /// Structured only: the absolute window end (shifts with pauses).
    pub window_end: Option<Unix>,
    // Flowmodoro bookkeeping -------------------------------------------------
    /// Focus seconds banked toward the next break, EXCLUDING the live stint.
    pub flow_banked: f64,
    /// When the current focus stint began (`None` while resting/paused).
    pub flow_stint_start: Option<Unix>,
    pub flow_breaks_claimed: i64,
}

impl Default for Session {
    /// The shape a lenient decode lands on when the state file predates a
    /// field: an over-but-finished session rather than a half-running one.
    fn default() -> Self {
        Self {
            config: Config::default(),
            started_at: 0,
            phase: Phase::Done,
            phase_start: None,
            phase_end: None,
            steps: Vec::new(),
            step_index: 0,
            paused_at: None,
            window_end: None,
            flow_banked: 0.0,
            flow_stint_start: None,
            flow_breaks_claimed: 0,
        }
    }
}

impl Session {
    /// Start a session: prep first, then the mode's timeline.
    pub fn start(config: Config, start: Unix) -> Self {
        let mut s = Self {
            config: config.clone(),
            started_at: start,
            phase: Phase::Prep,
            phase_start: Some(start),
            phase_end: Some(start + config.prep_seconds),
            ..Default::default()
        };
        if config.mode == Mode::Flowmodoro {
            // Open-ended focus after prep — nothing to schedule.
            if config.prep_seconds <= 0 {
                s.start_flow_focus(start);
            }
            return s;
        }
        if config.prep_seconds <= 0 {
            s.phase_end = Some(start);
        }
        s.steps = Self::build_timeline(&config, start);
        s.window_end = if config.mode == Mode::Structured {
            config.structured_window_end(start)
        } else {
            None
        };
        s.sync_current_step(start);
        s
    }

    /// The full structured/classic timeline: prep, then focus blocks with
    /// breaks interleaved. Structured ends exactly at the window end;
    /// classic runs purely off the configured lengths.
    fn build_timeline(config: &Config, start: Unix) -> Vec<Step> {
        let mut steps = Vec::new();
        // A structured window that cannot hold prep + the breaks + a focus
        // minute per segment builds no timeline at all — not even the prep
        // step (the start gate keeps such a config from ever running).
        if config.mode == Mode::Structured {
            let Some(window_end) = config.structured_window_end(start) else {
                return steps;
            };
            if config
                .structured_segment_seconds(window_end - start)
                .is_none()
            {
                return steps;
            }
        }
        let mut t = start;
        if config.prep_seconds > 0 {
            t = start + config.prep_seconds;
            steps.push(Step { phase: Phase::Prep, end: Some(t) });
        }
        match config.mode {
            Mode::Flowmodoro => return steps,
            Mode::Classic => {
                for (i, minutes) in config.classic_focus_minutes.iter().enumerate() {
                    t += minutes * 60;
                    steps.push(Step { phase: Phase::Focus, end: Some(t) });
                    if let Some(brk) = config.classic_break_minutes.get(i) {
                        t += brk * 60;
                        steps.push(Step { phase: Phase::Rest, end: Some(t) });
                    }
                }
            }
            Mode::Structured => {
                let Some(window_end) = config.structured_window_end(start) else {
                    return steps;
                };
                let Some(segment) = config.structured_segment_seconds(window_end - start) else {
                    return steps; // invalid window — the editor blocks starting it
                };
                for _ in 0..config.structured_break_count {
                    t += segment;
                    steps.push(Step { phase: Phase::Focus, end: Some(t) });
                    t += config.structured_break_seconds;
                    steps.push(Step { phase: Phase::Rest, end: Some(t) });
                }
                steps.push(Step { phase: Phase::Focus, end: Some(window_end) });
            }
        }
        steps
    }

    pub fn is_paused(&self) -> bool {
        self.paused_at.is_some()
    }

    pub fn is_running(&self) -> bool {
        self.phase != Phase::Done
    }

    /// The instant time is measured against: a paused session is frozen at
    /// the moment it paused, whatever the clock says now.
    fn anchor(&self, now: Unix) -> Unix {
        self.paused_at.unwrap_or(now)
    }

    /// Seconds left in the current phase at `now` — `None` for the
    /// open-ended flowmodoro focus and when done.
    pub fn seconds_remaining(&self, now: Unix) -> Option<i64> {
        let end = self.phase_end?;
        Some((end - self.anchor(now)).max(0))
    }

    /// 0..1 fill for the phase bar. Fixed-length phases fill by elapsed
    /// share; the open-ended flowmodoro focus fills cyclically toward the
    /// next earned break minute (300 s per lap).
    pub fn phase_progress(&self, now: Unix) -> f64 {
        match (self.phase_start, self.phase_end) {
            (Some(start), Some(end)) => {
                let total = end - start;
                if total <= 0 {
                    return 1.0;
                }
                let elapsed = (total).min((self.anchor(now) - start).max(0));
                elapsed as f64 / total as f64
            }
            (Some(_), None) => {
                let accrued = self.flow_accrued_focus(now);
                (accrued % FLOW_UNIT_FOCUS) / FLOW_UNIT_FOCUS
            }
            _ => 0.0,
        }
    }

    /// Flowmodoro: focus seconds banked toward the next break, including
    /// the live stint (frozen while paused).
    pub fn flow_accrued_focus(&self, now: Unix) -> f64 {
        let mut accrued = self.flow_banked;
        if self.phase == Phase::Focus {
            if let Some(stint) = self.flow_stint_start {
                accrued += (self.anchor(now) - stint).max(0) as f64;
            }
        }
        accrued.max(0.0)
    }

    /// Break seconds currently earned: 1 minute per 5 focus minutes.
    pub fn flow_earned_break(&self, now: Unix) -> f64 {
        self.flow_accrued_focus(now) / (FLOW_UNIT_FOCUS / 60.0)
    }

    pub fn flow_can_claim(&self, now: Unix) -> bool {
        self.config.mode == Mode::Flowmodoro
            && self.phase == Phase::Focus
            && !self.is_paused()
            && self.flow_accrued_focus(now) >= FLOW_CLAIM_THRESHOLD
    }

    /// Completes every phase whose end has passed by `now` and lands in the
    /// step that contains it — one call reconciles both a normal tick and a
    /// relaunch after the app was gone for hours. Returns the completed
    /// phases in order (durations included) for logging.
    pub fn advance_if_due(&mut self, now: Unix) -> Vec<Event> {
        if self.paused_at.is_some() || self.phase == Phase::Done {
            return Vec::new();
        }

        if self.config.mode == Mode::Flowmodoro {
            let Some(end) = self.phase_end else {
                return Vec::new(); // open-ended focus has no end to pass
            };
            if now < end {
                return Vec::new();
            }
            match self.phase {
                Phase::Prep | Phase::Rest => {
                    let was = self.phase;
                    let seconds = (end - self.phase_start.unwrap_or(end)) as f64;
                    self.start_flow_focus(end);
                    return vec![Event { completed_phase: was, seconds }];
                }
                _ => return Vec::new(),
            }
        }

        let mut events = Vec::new();
        while self.step_index < self.steps.len() {
            let Some(end) = self.steps[self.step_index].end else {
                break;
            };
            if now < end {
                break;
            }
            let start = if self.step_index == 0 {
                self.started_at
            } else {
                self.steps[self.step_index - 1].end.unwrap_or(self.started_at)
            };
            events.push(Event {
                completed_phase: self.steps[self.step_index].phase,
                seconds: (end - start) as f64,
            });
            self.step_index += 1;
        }
        self.sync_current_step(now);
        events
    }

    /// Points phase/phase_end at `steps[step_index]`; done when the
    /// timeline is exhausted. (Flowmodoro never has steps — untouched.)
    fn sync_current_step(&mut self, fallback_now: Unix) {
        if self.step_index >= self.steps.len() {
            self.phase = Phase::Done;
            self.phase_start = None;
            self.phase_end = None;
            return;
        }
        let step = &self.steps[self.step_index];
        self.phase = step.phase;
        self.phase_end = step.end;
        self.phase_start = if self.step_index == 0 {
            Some(self.started_at)
        } else {
            Some(self.steps[self.step_index - 1].end.unwrap_or(fallback_now))
        };
    }

    fn start_flow_focus(&mut self, at: Unix) {
        self.phase = Phase::Focus;
        self.phase_start = Some(at);
        self.phase_end = None;
        self.flow_stint_start = Some(at);
    }

    /// Flowmodoro: turn accrued focus into a break now. The break lasts the
    /// whole pot (1 min per 5 focus min) and the accumulator resets — focus
    /// starts from zero after it.
    pub fn claim_break(&mut self, now: Unix) -> bool {
        if self.config.mode != Mode::Flowmodoro
            || self.phase != Phase::Focus
            || self.is_paused()
            || self.flow_accrued_focus(now) < FLOW_CLAIM_THRESHOLD
        {
            return false;
        }
        // The pot's size, read before the reset — after `flow_banked = 0`
        // the accrual is gone and the break would last zero seconds.
        let rest = self.flow_earned_break(now);
        self.flow_banked = 0.0;
        self.flow_stint_start = None;
        self.flow_breaks_claimed += 1;
        self.phase = Phase::Rest;
        self.phase_start = Some(now);
        self.phase_end = Some(now + rest.round() as i64);
        true
    }

    /// End the current phase immediately.
    ///
    /// - structured: the window is absolute, so later boundaries stay put —
    ///   only the skipped phase ends early.
    /// - classic: the schedule slides — everything after the skipped phase
    ///   pulls forward by the reclaimed time.
    /// - flowmodoro: only a rest can be skipped (focus has no "next" without
    ///   a claim); focus resumes immediately.
    pub fn skip_phase(&mut self, now: Unix) -> bool {
        if self.phase == Phase::Done || self.is_paused() || now >= self.phase_end.unwrap_or(Unix::MAX) {
            return false;
        }
        match self.config.mode {
            Mode::Flowmodoro => {
                if self.phase != Phase::Rest {
                    return false;
                }
                self.start_flow_focus(now);
                true
            }
            Mode::Structured => {
                if self.step_index >= self.steps.len() {
                    return false;
                }
                self.steps[self.step_index].end = Some(now);
                true
            }
            Mode::Classic => {
                if self.step_index >= self.steps.len() {
                    return false;
                }
                let Some(end) = self.steps[self.step_index].end else {
                    return false;
                };
                let delta = end - now;
                self.steps[self.step_index].end = Some(now);
                for step in &mut self.steps[self.step_index + 1..] {
                    if let Some(e) = step.end.as_mut() {
                        *e -= delta;
                    }
                }
                true
            }
        }
    }

    /// Freeze the clock. Flow focus banks the live stint first so accrual
    /// stops exactly at the pause.
    pub fn pause(&mut self, now: Unix) -> bool {
        if self.phase == Phase::Done || self.is_paused() {
            return false;
        }
        if self.config.mode == Mode::Flowmodoro && self.phase == Phase::Focus {
            if let Some(stint) = self.flow_stint_start.take() {
                self.flow_banked += (now - stint).max(0) as f64;
            }
        }
        self.paused_at = Some(now);
        true
    }

    /// Slide every pending boundary by the paused span and unfreeze.
    pub fn resume(&mut self, now: Unix) -> bool {
        let Some(paused) = self.paused_at.take() else {
            return false;
        };
        let delta = (now - paused).max(0);
        if delta == 0 {
            return true;
        }
        if let Some(e) = self.phase_end.as_mut() {
            *e += delta;
        }
        if let Some(e) = self.window_end.as_mut() {
            *e += delta;
        }
        for step in &mut self.steps[self.step_index..] {
            if let Some(e) = step.end.as_mut() {
                *e += delta;
            }
        }
        if self.config.mode == Mode::Flowmodoro && self.phase == Phase::Focus {
            self.flow_stint_start = Some(now);
        }
        true
    }

    /// End the timer now. The phase in flight is reported with its partial
    /// duration so the store can fold focus seconds into the day log before
    /// dropping the session.
    pub fn stop(&mut self, now: Unix) -> Option<Event> {
        if self.phase == Phase::Done {
            return None;
        }
        let stopped_phase = self.phase;
        let seconds = if self.config.mode == Mode::Flowmodoro && self.phase == Phase::Focus {
            self.flow_accrued_focus(now)
        } else if let Some(start) = self.phase_start {
            let end = self.phase_end.unwrap_or(now).min(self.anchor(now));
            (end - start).max(0) as f64
        } else {
            0.0
        };
        self.phase = Phase::Done;
        self.phase_start = None;
        self.phase_end = None;
        self.paused_at = None;
        Some(Event { completed_phase: stopped_phase, seconds })
    }

    /// Break length that FOLLOWS the current focus phase, for copy —
    /// structured: the configured break; classic: the matching break entry;
    /// flow: the current pot.
    pub(crate) fn next_rest_seconds(&self) -> f64 {
        if self.config.mode == Mode::Flowmodoro {
            return 0.0;
        }
        let Some(next) = self.steps.get(self.step_index + 1) else {
            return 0.0;
        };
        if next.phase != Phase::Rest {
            return 0.0;
        }
        let Some(rest_end) = next.end else { return 0.0 };
        let focus_end = self.phase_end.unwrap_or(0);
        (rest_end - focus_end).max(0) as f64
    }
}

/// Human copy for durations that are often fractional — "36 s", "2.4 min".
pub fn break_length_text(seconds: f64) -> String {
    if seconds < 60.0 {
        return format!("{} s of break", (seconds.round() as i64).max(1));
    }
    let minutes = seconds / 60.0;
    if minutes >= 10.0 {
        format!("{} min of break", minutes.round() as i64)
    } else {
        format!("{minutes:.1} min of break")
    }
}

/// "1 h 30 min", "45 min".
pub fn minutes_text(seconds: i64) -> String {
    let minutes = (seconds as f64 / 60.0).round() as i64;
    if minutes >= 60 {
        format!("{} h {} m", minutes / 60, minutes % 60)
    } else {
        format!("{minutes} min")
    }
}

/// "3 h", "45 min" — whole minutes in, whole text out.
pub(crate) fn duration_text(seconds: i64) -> String {
    let h = seconds / 3600;
    let m = (seconds % 3600) / 60;
    if h > 0 && m > 0 {
        format!("{h} h {m} min")
    } else if h > 0 {
        format!("{h} h")
    } else {
        format!("{m} min")
    }
}

/// Countdown text; rounds up so the timer never shows 0:00 early.
pub fn countdown_text(seconds: f64) -> String {
    let total = seconds.max(0.0).ceil() as i64;
    if total >= 3600 {
        format!("{}:{:02}:{:02}", total / 3600, (total % 3600) / 60, total % 60)
    } else {
        format!("{:02}:{:02}", total / 60, total % 60)
    }
}

// ===========================================================================
// The store — the persisted half: config, the running session, the day log,
// and the banner. Port of PomodoroStore.swift, minus iOS's notification
// centre (the window is the app's whole world here; the banner is the
// announcement, and the chime is its voice).
// ===========================================================================
