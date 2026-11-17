//! The Pomodoro card's actions and the one-second heartbeat — start, stop,
//! pause, claim, skip, and the tick that advances the session and raises
//! the banners. Split out of `pomodoro.rs` (Round 4, pure code-motion).

use std::rc::Rc;


use crate::app::App;

use super::PomodoroWidgets;
use super::chime::chime;
use super::model::*;
use super::paint::{
    hide_banner, hide_banner_now, paint_banner, paint_card, BannerAction, BannerSpec,
};
use super::store::{now_unix, Store};


// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

/// Start (or replace) the running session with `config`. The config is
/// saved as the default, so the next open of the editor pre-fills it.
pub(crate) fn start_session(p: &PomodoroWidgets, app: &Rc<App>, config: Config) {
    let now = now_unix();
    {
        let mut store = p.store.borrow_mut();
        // A session already running is stopped honestly first: the phase in
        // flight folds its partial focus into today before it is replaced.
        if let Some(old) = store.session.as_mut() {
            if let Some(event) = old.stop(now) {
                store.fold(std::slice::from_ref(&event), now);
            }
        }
        store.config = config.clone();
        store.session = Some(Session::start(config.clone(), now));
        store.countdown_announced = None;
        store.claim_announced_for = None;
        if config.prep_seconds > 0 {
            let minutes = (config.prep_seconds as f64 / 60.0).round() as i64;
            store.banner = Some(BannerSpec {
                icon: "alarm-symbolic",
                tint: "prep",
                title: "Warming up".into(),
                subtitle: format!("{minutes} min of prep, then focus begins"),
                action: None,
                auto_dismiss: Some(8),
            });
        } else {
            store.banner = Some(resume_banner(false));
        }
        store.persist();
    }
    if config.prep_seconds > 0 {
        chime(config.chime);
    }
    paint_card(p, app);
    paint_banner(p);
}

pub(crate) fn stop_session(p: &PomodoroWidgets, app: &Rc<App>) {
    let now = now_unix();
    let mut store = p.store.borrow_mut();
    if let Some(old) = store.session.as_mut() {
        if let Some(event) = old.stop(now) {
            store.fold(std::slice::from_ref(&event), now);
        }
    }
    store.session = None;
    store.banner = None;
    store.persist();
    drop(store);
    hide_banner(p);
    paint_card(p, app);
    let _ = app;
}

pub(crate) fn toggle_pause(p: &PomodoroWidgets, app: &Rc<App>) {
    let now = now_unix();
    {
        let mut store = p.store.borrow_mut();
        let Some(session) = store.session.as_mut() else {
            return;
        };
        if session.is_paused() {
            session.resume(now);
        } else {
            session.pause(now);
        }
        store.persist();
    }
    paint_card(p, app);
    let _ = app;
}

/// Flowmodoro: bank the pot as a break now.
pub(crate) fn claim_break(p: &PomodoroWidgets, app: &Rc<App>) {
    let now = now_unix();
    let earned;
    let chime_on;
    {
        let mut store = p.store.borrow_mut();
        let Some(session) = store.session.as_mut() else {
            return;
        };
        earned = break_length_text(session.flow_earned_break(now));
        chime_on = session.config.chime;
        if !session.claim_break(now) {
            return;
        }
        store.claim_announced_for = None;
        store.banner = Some(BannerSpec {
            icon: "pomodoro-cup-symbolic",
            tint: "rest",
            title: earned.clone(),
            subtitle: "To yourself — the pot starts refilling after".into(),
            action: None,
            auto_dismiss: Some(8),
        });
        store.persist();
    }
    chime(chime_on);
    paint_card(p, app);
    paint_banner(p);
    let _ = earned;
}

/// Skip the current phase. Structured keeps its absolute window; classic
/// slides; flow skips a rest. The engine's advance after the skip emits
/// the partial-focus event, so history stays honest.
pub(crate) fn skip_phase(p: &PomodoroWidgets, app: &Rc<App>) {
    let now = now_unix();
    {
        let mut store = p.store.borrow_mut();
        let Some(session) = store.session.as_mut() else {
            return;
        };
        if !session.skip_phase(now) {
            return;
        }
    }
    // The transition the skip produced is announced by the same tick path
    // as a natural one — one code path, so the copy cannot drift.
    tick(p, app);
}

/// "End this pomodoro?" — the stop button's confirm, because a timer that
/// dies to a stray click has no sense of occasion. Focus time so far stays
/// in today's count.
pub(crate) fn confirm_stop(p: &PomodoroWidgets, app: &Rc<App>) {
    let dialog = gtk4::AlertDialog::builder()
        .message("End this pomodoro?")
        .detail("Focus time so far stays in today's count.")
        .modal(true)
        .build();
    dialog.set_buttons(&["Keep going", "End timer"]);
    let widgets = p.clone();
    let app = app.clone();
    let parent = app
        .widgets
        .borrow()
        .as_ref()
        .map(|w| w.window.clone());
    dialog.choose(
        parent.as_ref(),
        None::<&gtk4::gio::Cancellable>,
        move |result| {
            // 0 = Keep going, 1 = End timer.
            if matches!(result, Ok(1)) {
                stop_session(&widgets, &app);
            }
        },
    );
}

// ---------------------------------------------------------------------------
// The tick and the announcements
// ---------------------------------------------------------------------------

/// The one-second heartbeat. Advances the session, folds transitions into
/// the day log, raises the matching banner, and keeps the card honest.
pub(crate) fn tick(p: &PomodoroWidgets, app: &Rc<App>) {
    let now = now_unix();
    let mut store = p.store.borrow_mut();
    let Some(current) = store.session.as_ref() else {
        drop(store);
        paint_card(p, app);
        return;
    };
    if current.is_paused() {
        drop(store);
        paint_card(p, app);
        return;
    }
    let mut session = current.clone();

    let events = session.advance_if_due(now);
    if events.is_empty() {
        // No transition — the announcements may still have fired (the
        // one-minute nudge, the flow claim hint): the banner paints if one
        // did, and the card repaints its clock either way.
        store.session = Some(session);
        let before = store.banner.is_some();
        announce_countdown(&mut store, now);
        announce_claim(&mut store, now);
        let changed = store.banner.is_some() != before;
        drop(store);
        paint_card(p, app);
        if changed {
            paint_banner(p);
        }
        return;
    }

    let finished = session.phase == Phase::Done;
    store.fold(&events, now);
    store.countdown_announced = None;
    let chime_on = session.config.chime;
    if finished {
        store.session = None;
        store.persist();
        drop(store);
        // Built after the borrow_mut is gone — it reads today's record
        // through its own store borrow.
        let banner = session_complete_banner(p, app);
        p.store.borrow_mut().banner = Some(banner);
        hide_banner_now(p);
        chime(chime_on);
        paint_card(p, app);
        paint_banner(p);
        return;
    }
    // The phase that just began gets the gentle headline.
    let banner = match session.phase {
        Phase::Focus => {
            let from_break = events.iter().any(|e| e.completed_phase == Phase::Rest);
            resume_banner(from_break)
        }
        Phase::Rest => {
            let rest = session.next_rest_seconds();
            BannerSpec {
                icon: "object-select-symbolic",
                tint: "rest",
                title: "Nice work — block done".into(),
                subtitle: format!(
                    "{earned} starting now — stretch, sip, breathe",
                    earned = if rest > 0.0 {
                        break_length_text(rest)
                    } else {
                        "a short breather".to_string()
                    }
                ),
                action: None,
                auto_dismiss: Some(8),
            }
        }
        // Prep never *begins* from a transition; it begins from start().
        Phase::Prep | Phase::Done => {
            store.session = Some(session);
            store.persist();
            drop(store);
            paint_card(p, app);
            return;
        }
    };
    store.session = Some(session);
    store.banner = Some(banner);
    store.persist();
    drop(store);
    chime(chime_on);
    paint_card(p, app);
    paint_banner(p);
}

/// The "last minutes" nudge — one per phase, at the 60-second mark.
pub(crate) fn announce_countdown(store: &mut Store, now: Unix) {
    let Some(session) = store.session.as_ref() else {
        return;
    };
    let Some(end) = session.phase_end else {
        return; // open-ended flow focus never counts down
    };
    let remaining = end - now;
    if remaining <= 0 || remaining > 60 {
        return;
    }
    let key = format!("{}-{:?}-{end}", session.step_index, session.phase);
    if store.countdown_announced.as_deref() == Some(key.as_str()) {
        return;
    }
    store.countdown_announced = Some(key);
    let subtitle = match session.phase {
        Phase::Prep => "Focus begins in under a minute",
        Phase::Focus => "Finish the thought — a break is close",
        Phase::Rest => "Ease back toward your reading when ready",
        Phase::Done => return,
    };
    store.banner = Some(BannerSpec {
        icon: "pomodoro-hourglass-symbolic",
        tint: session.phase.css(),
        title: "About a minute left".into(),
        subtitle: subtitle.into(),
        action: None,
        auto_dismiss: Some(10),
    });
}

/// Flowmodoro: the pot crossed 3 focus minutes — offer the claim once per
/// stint, softly, with the claim one tap away.
pub(crate) fn announce_claim(store: &mut Store, now: Unix) {
    let Some(session) = store.session.as_ref() else {
        return;
    };
    if session.config.mode != Mode::Flowmodoro || session.phase != Phase::Focus {
        return;
    }
    let Some(stint) = session.flow_stint_start else {
        return;
    };
    if store.claim_announced_for == Some(stint) || !session.flow_can_claim(now) {
        return;
    }
    store.claim_announced_for = Some(stint);
    store.banner = Some(BannerSpec {
        icon: "pomodoro-cup-symbolic",
        tint: "rest",
        title: format!("{} banked", break_length_text(session.flow_earned_break(now))),
        subtitle: "Claim a break whenever you like — it keeps growing".into(),
        action: Some(BannerAction::ClaimBreak),
        auto_dismiss: None,
    });
}

pub(crate) fn resume_banner(from_break: bool) -> BannerSpec {
    BannerSpec {
        icon: "view-contents-symbolic",
        tint: "focus",
        title: if from_break { "Break's over" } else { "Ready to focus" }.into(),
        subtitle: if from_break {
            "Ease back into your reading — no rush".into()
        } else {
            "Your prep time is up — ease into the first block".into()
        },
        action: None,
        auto_dismiss: if from_break { None } else { Some(7) },
    }
}

pub(crate) fn session_complete_banner(p: &PomodoroWidgets, app: &Rc<App>) -> BannerSpec {
    let record = {
        let store = p.store.borrow();
        store.today(now_unix())
    };
    let _ = app;
    BannerSpec {
        icon: "starred-symbolic",
        tint: "gold",
        title: "Pomodoro complete".into(),
        subtitle: if record.blocks > 0 {
            format!(
                "{} block{} · {} focused — well done today",
                record.blocks,
                if record.blocks == 1 { "" } else { "s" },
                minutes_text(record.focus_seconds as i64)
            )
        } else {
            "Well done today".into()
        },
        action: None,
        auto_dismiss: Some(12),
    }
}
