//! The Pomodoro state file and the live store behind the card — the port
//! of `PomodoroStore.swift`. Split out of `pomodoro.rs` (Round 4, pure
//! code-motion).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::model::{Config, Event, Phase, Session, Unix};
use super::paint::BannerSpec;

/// One day of folded focus time — the timer's own "today" line. Deliberately
/// not a stats-store entry: reading/listening charts split by kind, and
/// timer minutes are neither.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct DayRecord {
    pub day_key: String,
    pub focus_seconds: f64,
    pub blocks: i64,
}

/// The persisted state: last-used config, the running session (if any), and
/// the day-folded focus history. One small JSON file — the reading-goals
/// pattern. Every field is optional on decode, so a state file written by an
/// older build restores rather than wipes.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StateFile {
    pub(crate) config: Option<Config>,
    pub(crate) session: Option<Session>,
    #[serde(default)]
    pub(crate) history: Vec<DayRecord>,
}

pub(crate) fn state_path() -> PathBuf {
    sn_core::paths::data_dir().join("pomodoro-state.json")
}

pub(crate) fn now_unix() -> Unix {
    sn_core::appledate::unix_now() as i64
}

pub(crate) struct Store {
    pub(crate) config: Config,
    pub(crate) session: Option<Session>,
    pub(crate) history: Vec<DayRecord>,
    pub(crate) banner: Option<BannerSpec>,
    /// "1 minute left" fires once per phase — keyed by step index + phase +
    /// the phase end, so a repeat in the same phase cannot re-fire.
    pub(crate) countdown_announced: Option<String>,
    /// The flow claim hint fires once per focus stint — keyed by the stint's
    /// start instant.
    pub(crate) claim_announced_for: Option<Unix>,
    /// Set by the loader when a break ended (or the session finished) while
    /// the app was closed; the widget half turns it into the matching
    /// banner the moment the window exists.
    reconcile_news: Option<ReconcileNews>,
}

/// What happened while the app was closed, for the banner on reopen.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum ReconcileNews {
    /// The whole window ran out while away.
    SessionDone,
    /// A break ended and focus is waiting.
    BreakOver,
}

impl Store {
    pub(crate) fn load() -> Self {
        let (loaded, _) =
            sn_core::atomic::load_json_recoverable::<StateFile>(&state_path());
        let mut store = Self {
            config: loaded.config.unwrap_or_default(),
            session: None,
            history: loaded.history,
            banner: None,
            countdown_announced: None,
            claim_announced_for: None,
            reconcile_news: None,
        };
        // A session that ran while the app was closed is reconciled from the
        // wall clock alone: every phase that elapsed while away folds into
        // the day log without chimes — the banner the reader sees on
        // reopening IS the announcement for it.
        if let Some(mut session) = loaded.session {
            let now = now_unix();
            let events = session.advance_if_due(now);
            let finished = session.phase == Phase::Done;
            store.fold(&events, now);
            store.reconcile_news = if finished {
                Some(ReconcileNews::SessionDone)
            } else if events.iter().any(|e| e.completed_phase == Phase::Rest) {
                Some(ReconcileNews::BreakOver)
            } else {
                None
            };
            if finished {
                store.session = None;
            } else {
                store.session = Some(session);
            }
        }
        store
    }

    pub(crate) fn persist(&self) {
        let file = StateFile {
            config: Some(self.config.clone()),
            session: self.session.clone(),
            history: self.history.clone(),
        };
        if let Err(e) = sn_core::atomic::write_json_atomic(&state_path(), &file) {
            tracing::warn!(error = %e, "the pomodoro state could not be saved");
        }
    }

    /// Fold focus events into the day log: one record per local day, newest
    /// first, ninety days kept.
    pub(crate) fn fold(&mut self, events: &[Event], now: Unix) {
        let focus: Vec<&Event> = events
            .iter()
            .filter(|e| e.completed_phase == Phase::Focus && e.seconds > 0.5)
            .collect();
        if focus.is_empty() {
            return;
        }
        let key = sn_core::AppleDate::local_day_from_unix(now);
        let mut record = self
            .history
            .iter()
            .find(|r| r.day_key == key)
            .cloned()
            .unwrap_or(DayRecord { day_key: key.clone(), focus_seconds: 0.0, blocks: 0 });
        for event in focus {
            record.focus_seconds += event.seconds;
            record.blocks += 1;
        }
        self.history.retain(|r| r.day_key != key);
        self.history.insert(0, record);
        self.history.truncate(90);
    }

    pub(crate) fn today(&self, now: Unix) -> DayRecord {
        let key = sn_core::AppleDate::local_day_from_unix(now);
        self.history
            .iter()
            .find(|r| r.day_key == key)
            .cloned()
            .unwrap_or_default()
    }
}

impl Store {
    /// What happened while the app was closed, taken once by the widget
    /// half: the banner should be raised exactly on reopen, not re-raised
    /// by every later tick.
    pub(crate) fn take_reconcile_news(&mut self) -> Option<ReconcileNews> {
        self.reconcile_news.take()
    }
}

// ===========================================================================
// The widget half — the card, the banner, the config popover, and the
// app-wide one-second tick.
// ===========================================================================
