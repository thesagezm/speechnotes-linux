//! Painting the Pomodoro card and raising the gentle banner — the port of
//! `PomodoroViews.swift`'s update paths. Split out of `pomodoro.rs`
//! (Round 4, pure code-motion).

use std::rc::Rc;

use gtk4::glib;
use gtk4::prelude::*;
use chrono::TimeZone;

use crate::app::App;

use super::PomodoroWidgets;
use super::model::*;
use super::store::now_unix;

/// What the banner says, and what it does when the reader taps its action.
/// Pure data — the widget half renders it; the kinds map to copy here so
/// every raiser of the same news says it the same way.
#[derive(Clone)]
pub(crate) struct BannerSpec {
    pub(crate) icon: &'static str,
    /// The tint class: `prep`, `focus`, `rest`, or `gold` (celebrations).
    pub(crate) tint: &'static str,
    pub(crate) title: String,
    pub(crate) subtitle: String,
    pub(crate) action: Option<BannerAction>,
    /// `None` = stays until dismissed (still gentle: small, top,
    /// tappable-away).
    pub(crate) auto_dismiss: Option<u64>,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum BannerAction {
    /// Flowmodoro's "Claim now".
    ClaimBreak,
}

/// The timer's live state, behind one `RefCell` the tick and every button
/// share.

// ---------------------------------------------------------------------------
// Painting
// ---------------------------------------------------------------------------

/// Paint the card from the store: which page, the clock, the phase pill,
/// the caption, the controls' sensitivity, the today line.
pub(crate) fn paint_card(p: &PomodoroWidgets, app: &Rc<App>) {
    let now = now_unix();
    let (config, session) = {
        let store = p.store.borrow();
        (store.config.clone(), store.session.clone())
    };

    match &session {
        None => {
            p.stack.set_visible_child_name("idle");
            let now = now;
            p.idle_summary.set_text(&match config.mode {
                Mode::Structured => {
                    let when = if config.structured_by_duration {
                        duration_text(config.structured_duration_seconds)
                    } else {
                        format!(
                            "{:02}:{:02}",
                            config.structured_finish_hour, config.structured_finish_minute
                        )
                    };
                    format!(
                        "Structured — one window {}, {} break{} spread through it.",
                        when,
                        config.structured_break_count,
                        if config.structured_break_count == 1 { "" } else { "s" }
                    )
                }
                Mode::Flowmodoro => {
                    "Flowmodoro — focus as long as you like; 5 min earns 1 min of break."
                        .to_string()
                }
                Mode::Classic => {
                    let focus: i64 = config.classic_focus_minutes.iter().sum();
                    format!(
                        "Classic — {} block{} · {} of focus, breaks in between.",
                        config.classic_focus_minutes.len(),
                        if config.classic_focus_minutes.len() == 1 { "" } else { "s" },
                        minutes_text(focus * 60)
                    )
                }
            });
            p.idle_start.set_sensitive(config.can_start(now));
            let tip = if config.can_start(now) {
                None
            } else {
                Some("This window cannot fit the plan — open the settings to fix it.".to_string())
            };
            p.idle_start.set_tooltip_text(tip.as_deref());
        }
        Some(session) => {
            p.stack.set_visible_child_name("run");
            let paused = session.is_paused();

            // The clock: fixed phases count down; flowmodoro's open focus
            // counts up (the pot).
            if session.config.mode == Mode::Flowmodoro && session.phase == Phase::Focus {
                p.run_time.set_text(&countdown_text(session.flow_accrued_focus(now)));
            } else {
                let left = session.seconds_remaining(now).unwrap_or(0) as f64;
                p.run_time.set_text(&countdown_text(left));
            }
            let mut time_classes = vec!["pomodoro-time", "numeric"];
            if paused {
                time_classes.push("paused");
            }
            p.run_time.set_css_classes(&time_classes);

            // The phase pill: which block, which break, or paused.
            p.run_phase.set_text(&phase_pill_text(session, paused));
            for class in ["prep", "focus", "rest", "done"] {
                p.run_phase.remove_css_class(class);
            }
            p.run_phase.add_css_class(if paused { "done" } else { session.phase.css() });

            // The bar's fill and its tint. The phase class goes on the
            // ProgressBar itself — CSS cannot select a parent, so a tint
            // on the container would never reach the fill.
            p.run_bar.set_fraction(session.phase_progress(now).clamp(0.0, 1.0));
            for class in ["prep", "focus", "rest", "done"] {
                p.run_bar.remove_css_class(class);
            }
            p.run_bar.add_css_class(session.phase.css());

            // The caption: the mode's shape, in one line.
            p.run_caption.set_text(&match session.config.mode {
                Mode::Flowmodoro => {
                    let earned = break_length_text(session.flow_earned_break(now));
                    format!("5 min of focus earns 1 min of break · {earned} in the pot")
                }
                Mode::Structured => {
                    let end = session
                        .window_end
                        .map(|end| {
                            chrono::Local
                                .timestamp_opt(end, 0)
                                .single()
                                .map(|local| format!("{}:{:02}", local.format("%H"), local.format("%M")))
                                .unwrap_or_default()
                        })
                        .unwrap_or_default();
                    format!(
                        "Ends {end} · {} break{} × {} min",
                        session.config.structured_break_count,
                        if session.config.structured_break_count == 1 { "" } else { "s" },
                        session.config.structured_break_seconds / 60
                    )
                }
                Mode::Classic => {
                    let focus: i64 = session.config.classic_focus_minutes.iter().sum();
                    format!(
                        "{} blocks · {} breaks · {} of focus",
                        session.config.classic_focus_minutes.len(),
                        session.config.classic_break_minutes.len(),
                        minutes_text(focus * 60)
                    )
                }
            });

            // Controls: pause/resume; the claim only in flow focus, live
            // only once the pot pays out; skip never during the mandatory
            // prep, and for flowmodoro only a rest has a "next".
            //
            // The button's *name* flips with the state, not just its icon:
            // an icon-only button tells a screen reader "button" and
            // nothing else, and the tooltip is what a sighted mouse
            // user reads. "Pause" while paused is worse than no label.
            let pause_label = if paused { "Resume" } else { "Pause" };
            p.pause_btn.set_icon_name(if paused {
                "media-playback-start-symbolic"
            } else {
                "media-playback-pause-symbolic"
            });
            p.pause_btn.set_tooltip_text(Some(pause_label));
            p.pause_btn.update_property(&[gtk4::accessible::Property::Label(
                pause_label,
            )]);
            let flow_focus = session.config.mode == Mode::Flowmodoro && session.phase == Phase::Focus;
            p.claim_btn.set_visible(flow_focus);
            p.claim_btn.set_sensitive(flow_focus && session.flow_can_claim(now));
            let can_skip = !paused
                && session.phase != Phase::Prep
                && match session.config.mode {
                    Mode::Flowmodoro => session.phase == Phase::Rest,
                    _ => true,
                };
            p.skip_btn.set_visible(can_skip);
        }
    }

    // The today line: only when today has something in it.
    let today = {
        let store = p.store.borrow();
        store.today(now_unix())
    };
    if today.blocks > 0 || today.focus_seconds > 0.0 {
        p.today_label.set_text(&format!(
            "Today · {} block{} · {} focused",
            today.blocks,
            if today.blocks == 1 { "" } else { "s" },
            minutes_text(today.focus_seconds as i64)
        ));
        p.today_label.set_visible(true);
    } else {
        p.today_label.set_visible(false);
    }
    let _ = app;
}

/// "Focus 2 of 4", "Break 1 of 3", "Preparing", "Paused" — which segment of
/// the schedule is live, numbered for the scheduled modes.
pub(crate) fn phase_pill_text(session: &Session, paused: bool) -> String {
    if paused {
        return "Paused".to_string();
    }
    match session.phase {
        Phase::Prep => "Preparing".to_string(),
        Phase::Done => "Done".to_string(),
        Phase::Focus if session.config.mode == Mode::Flowmodoro => "Focus".to_string(),
        Phase::Rest if session.config.mode == Mode::Flowmodoro => "Break".to_string(),
        Phase::Focus | Phase::Rest => {
            let phase = session.phase;
            let total = session.steps.iter().filter(|s| s.phase == phase).count();
            if total == 0 {
                return phase.label().to_string();
            }
            let through = session.step_index.min(session.steps.len().saturating_sub(1));
            let n = session.steps[..=through]
                .iter()
                .filter(|s| s.phase == phase)
                .count();
            format!("{} {} of {}", phase.label(), n.max(1), total)
        }
    }
}

/// Paint the banner widget from the store's spec: content, tint, action,
/// and the auto-dismiss timer. Replacing a banner replaces its timer.
pub(crate) fn paint_banner(p: &PomodoroWidgets) {
    let Some(spec) = p.store.borrow().banner.clone() else {
        hide_banner(p);
        return;
    };
    p.banner_icon.set_icon_name(Some(spec.icon));
    p.banner_title.set_text(&spec.title);
    p.banner_body.set_text(&spec.subtitle);
    for class in ["prep", "focus", "rest", "gold"] {
        p.banner_box.remove_css_class(class);
    }
    p.banner_box.add_css_class(spec.tint);
    match spec.action {
        Some(BannerAction::ClaimBreak) => {
            p.banner_action.set_label("Claim now");
            p.banner_action.set_visible(true);
        }
        None => p.banner_action.set_visible(false),
    }
    p.banner.set_reveal_child(true);
    if let Some(old) = p.banner_source.borrow_mut().take() {
        old.remove();
    }
    if let Some(secs) = spec.auto_dismiss {
        let widgets = p.clone();
        let source = glib::timeout_add_local(std::time::Duration::from_secs(secs), move || {
            *widgets.banner_source.borrow_mut() = None;
            hide_banner(&widgets);
            glib::ControlFlow::Break
        });
        *p.banner_source.borrow_mut() = Some(source);
    }
}

pub(crate) fn show_banner(p: &PomodoroWidgets, spec: BannerSpec) {
    p.store.borrow_mut().banner = Some(spec);
    paint_banner(p);
}

pub(crate) fn hide_banner(p: &PomodoroWidgets) {
    if let Some(old) = p.banner_source.borrow_mut().take() {
        old.remove();
    }
    p.store.borrow_mut().banner = None;
    p.banner.set_reveal_child(false);
}

/// Take the current banner down without clearing its spec — used on the
/// session-complete path, which paints a fresh one right after and does not
/// want the old timer to fire mid-swap.
pub(crate) fn hide_banner_now(p: &PomodoroWidgets) {
    if let Some(old) = p.banner_source.borrow_mut().take() {
        old.remove();
    }
    p.banner.set_reveal_child(false);
}
