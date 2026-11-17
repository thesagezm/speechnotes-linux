//! The Pomodoro timer: three modes, a card in the sidebar's foot, and the
//! gentle banner when something happens.
//!
//! This is a port of the iOS app's timer (`speechnotes-ios`
//! `PomodoroEngine.swift` + `PomodoroStore.swift` + `PomodoroViews.swift`),
//! because the user asked for feature parity with it — "structured",
//! "flowmodoro" and all:
//!
//! * **Structured** — one fixed window (a duration from start, or a
//!   wall-clock finish time) with N breaks spread evenly through it. The
//!   window is *absolute*: pauses slide it, but skips do not pull the
//!   finish earlier.
//! * **Flowmodoro** — open-ended focus; every 5 focus minutes bank 1 break
//!   minute, claimable from 3 focus minutes. The pot resets on a claim.
//! * **Classic** — hand-tuned focus blocks and breaks, each with its own
//!   length (25 then 30, 5 then 15, whatever fits).
//!
//! Every mode opens with a mandatory 2–5 minute prep window.
//!
//! The engine is *pure date math*: every boundary is a wall-clock instant,
//! never a tick count, so the session survives the app being closed —
//! on relaunch the persisted session is fed the current time through
//! [`Session::advance_if_due`] and every phase that elapsed while away is
//! reconciled (and folded into the day log) from the timeline alone. The
//! same call reconciles a normal one-second tick.
//!
//! History is day-folded focus time — the timer's own "today" line, the
//! proof it feeds something. The state lives in one small JSON file next
//! to the notes store (`pomodoro-state.json`), the reading-goals pattern.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4::glib;
use gtk4::prelude::*;

use crate::app::App;

mod chime;
mod config;
mod model;
mod paint;
mod session;
mod store;

// Re-exported wholesale so every existing path (`crate::pomodoro::build`,
// `crate::pomodoro::PomodoroWidgets`, the engine types the rest of the app
// reads) keeps resolving unchanged. Glob re-exports preserve each item's
// own visibility.
pub(crate) use config::*;
pub use model::*;
pub(crate) use paint::*;
pub(crate) use session::*;
pub use store::*;

/// The card, the banner, and the store behind them.
///
/// `Clone` shares everything through `Rc` — clones are handed to signal
/// closures, and two copies of a timer would run two pomodoros.
#[derive(Clone)]
pub struct PomodoroWidgets {
    /// The card, dropped into the sidebar above the Bin by
    /// `refresh_sidebar`; the sidebar rebuild re-appends this same widget,
    /// so the running timer survives every structural refresh.
    pub card: gtk4::Box,
    /// The gentle banner, an overlay child over the page area, so it is
    /// visible in fill-the-window mode too — when the reader's eyes are on
    /// the book is exactly when a phase ends.
    pub banner: gtk4::Revealer,
    pub(crate) store: Rc<RefCell<Store>>,
    pub(crate) stack: gtk4::Stack,
    pub(crate) idle_summary: gtk4::Label,
    pub(crate) idle_start: gtk4::Button,
    pub(crate) run_bar: gtk4::ProgressBar,
    pub(crate) run_time: gtk4::Label,
    pub(crate) run_phase: gtk4::Label,
    pub(crate) run_caption: gtk4::Label,
    pub(crate) pause_btn: gtk4::Button,
    pub(crate) claim_btn: gtk4::Button,
    pub(crate) skip_btn: gtk4::Button,
    pub(crate) stop_btn: gtk4::Button,
    pub(crate) today_label: gtk4::Label,
    pub(crate) banner_icon: gtk4::Image,
    pub(crate) banner_text: gtk4::Box,
    pub(crate) banner_title: gtk4::Label,
    pub(crate) banner_body: gtk4::Label,
    pub(crate) banner_action: gtk4::Button,
    pub(crate) banner_dismiss: gtk4::Button,
    pub(crate) banner_box: gtk4::Box,
    pub(crate) banner_source: Rc<RefCell<Option<glib::SourceId>>>,
}

/// Build the card and the banner. Neither is parented here: `window.rs`
/// puts the banner on its page overlay and `refresh_sidebar` puts the card
/// in the sidebar, every time it rebuilds.
pub fn build(app: &Rc<App>) -> PomodoroWidgets {
    let store = Rc::new(RefCell::new(Store::load()));

    // ---- the card ----------------------------------------------------------
    let card = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(5)
        .css_classes(["pomodoro-card"])
        .margin_top(4)
        .margin_bottom(2)
        .margin_start(6)
        .margin_end(6)
        .build();

    let title = gtk4::Label::builder()
        .label("Pomodoro")
        .halign(gtk4::Align::Start)
        .css_classes(["pomodoro-title"])
        .build();
    let gear = gtk4::MenuButton::builder()
        .icon_name("emblem-system-symbolic")
        .css_classes(["flat"])
        .build();
    gear.set_tooltip_text(Some("Set up the pomodoro timer"));
    // A MenuButton's accessible name is derived from its popover's contents,
    // so once the editor opens the gear announces the whole form. Pin the
    // name to the tooltip: the gear is a button, not a summary.
    gear.update_property(&[gtk4::accessible::Property::Label(
        "Set up the pomodoro timer",
    )]);

    let top = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(4)
        .build();
    top.append(&title);
    let spacer = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    top.append(&spacer);
    top.append(&gear);

    // Idle: no timer running — the last config's shape, and one click to
    // start it again.
    //
    // The summary wraps, and a capped width keeps it from pinning the
    // sidebar's minimum: a wrapped label's minimum is still the width of
    // its longest word, and "Flowmodoro — focus as long as you like; 5 min
    // earns 1 min of break." is a long line. The cap is what lets the
    // sidebar shrink when the window does.
    let idle_summary = gtk4::Label::builder()
        .halign(gtk4::Align::Start)
        .wrap(true)
        .max_width_chars(28)
        .css_classes(["pomodoro-summary"])
        .build();
    let idle_start = gtk4::Button::builder()
        .label("Start")
        .css_classes(["pomodoro-start", "suggested-action"])
        .halign(gtk4::Align::Fill)
        .build();
    let idle = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(7)
        .build();
    idle.append(&idle_summary);
    idle.append(&idle_start);

    // Running: the phase plate (a fill that grows with the phase, the shape
    // the iOS card settled on after its ring read "round" and sat
    // off-center), the clock and the phase pill centred on it, the caption,
    // and the controls.
    let run_time = gtk4::Label::builder()
        .css_classes(["pomodoro-time", "numeric"])
        .halign(gtk4::Align::Center)
        .build();
    let run_phase = gtk4::Label::builder()
        .css_classes(["pomodoro-phase-pill"])
        .halign(gtk4::Align::Center)
        .build();
    let run_stack_text = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(2)
        .halign(gtk4::Align::Center)
        .valign(gtk4::Align::Center)
        .build();
    run_stack_text.append(&run_time);
    run_stack_text.append(&run_phase);

    let run_bar = gtk4::ProgressBar::builder()
        .css_classes(["pomodoro-bar", "rest"])
        .fraction(0.0)
        .valign(gtk4::Align::Fill)
        .hexpand(true)
        .build();
    let run_bar_box = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    run_bar_box.append(&run_bar);
    let run_fill = gtk4::Overlay::builder()
        .css_classes(["pomodoro-fill"])
        .build();
    run_fill.set_child(Some(&run_bar_box));
    run_fill.add_overlay(&run_stack_text);

    let run_caption = gtk4::Label::builder()
        .halign(gtk4::Align::Center)
        .wrap(true)
        .css_classes(["pomodoro-caption"])
        .build();

    let pause_btn = gtk4::Button::builder()
        .icon_name("media-playback-pause-symbolic")
        .css_classes(["pomodoro-play"])
        .tooltip_text("Pause")
        .build();
    let claim_btn = gtk4::Button::builder()
        .icon_name("pomodoro-cup-symbolic")
        .label("Claim")
        .css_classes(["pomodoro-claim"])
        .tooltip_text("Claim the break you have banked")
        .build();
    let skip_btn = gtk4::Button::builder()
        .icon_name("media-skip-forward-symbolic")
        .tooltip_text("Skip this phase")
        .build();
    let stop_btn = gtk4::Button::builder()
        .icon_name("media-playback-stop-symbolic")
        .css_classes(["destructive-action"])
        .tooltip_text("End the timer")
        .build();
    // An icon-only button has no accessible name of its own: a screen
    // reader announces "button" and stops. The tooltip is the human
    // wording, so it is the name too.
    for (button, name) in [
        (&pause_btn, "Pause"),
        (&claim_btn, "Claim the break you have banked"),
        (&skip_btn, "Skip this phase"),
        (&stop_btn, "End the timer"),
    ] {
        button.update_property(&[gtk4::accessible::Property::Label(name)]);
    }
    let controls = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk4::Align::Center)
        .css_classes(["pomodoro-controls"])
        .build();
    controls.append(&pause_btn);
    controls.append(&claim_btn);
    controls.append(&skip_btn);
    controls.append(&stop_btn);

    let run = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(7)
        .build();
    run.append(&run_fill);
    run.append(&run_caption);
    run.append(&controls);

    let stack = gtk4::Stack::builder()
        .transition_type(gtk4::StackTransitionType::None)
        .build();
    stack.add_titled(&idle, Some("idle"), "Idle");
    stack.add_titled(&run, Some("run"), "Running");

    // The "today" line — proof the timer feeds something. Hidden until the
    // day has focus time in it.
    //
    // It wraps with a capped width: the line reads "Today · 1 block · 4 h
    // 51 m focused", and without the cap its natural width pinned the
    // sidebar's minimum width.
    let today_label = gtk4::Label::builder()
        .halign(gtk4::Align::Start)
        .wrap(true)
        .max_width_chars(28)
        .css_classes(["pomodoro-today"])
        .build();

    card.append(&top);
    card.append(&stack);
    card.append(&today_label);

    // ---- the banner ---------------------------------------------------------
    let banner = gtk4::Revealer::builder()
        .transition_type(gtk4::RevealerTransitionType::SlideDown)
        .transition_duration(320)
        .reveal_child(false)
        .halign(gtk4::Align::Center)
        .valign(gtk4::Align::Start)
        .margin_top(10)
        .build();
    let banner_icon = gtk4::Image::builder()
        .css_classes(["banner-icon"])
        .build();
    let banner_title = gtk4::Label::builder()
        .css_classes(["banner-title"])
        .halign(gtk4::Align::Start)
        .build();
    let banner_body = gtk4::Label::builder()
        .css_classes(["banner-body"])
        .halign(gtk4::Align::Start)
        .wrap(true)
        .max_width_chars(46)
        .build();
    let banner_text = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(1)
        .build();
    banner_text.append(&banner_title);
    banner_text.append(&banner_body);
    let banner_action = gtk4::Button::builder()
        .css_classes(["banner-action"])
        .visible(false)
        .build();
    let banner_dismiss = gtk4::Button::builder()
        .icon_name("window-close-symbolic")
        .css_classes(["flat", "banner-dismiss"])
        .build();
    let banner_box = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(11)
        .css_classes(["gentle-banner"])
        .build();
    banner_box.append(&banner_icon);
    banner_box.append(&banner_text);
    banner_box.append(&banner_action);
    banner_box.append(&banner_dismiss);
    banner.set_child(Some(&banner_box));

    let widgets = PomodoroWidgets {
        card,
        banner,
        stack,
        idle_summary,
        idle_start,
        run_bar,
        run_time,
        run_phase,
        run_caption,
        pause_btn,
        claim_btn,
        skip_btn,
        stop_btn,
        today_label,
        banner_icon,
        banner_text,
        banner_title,
        banner_body,
        banner_action,
        banner_dismiss,
        banner_box,
        banner_source: Rc::new(RefCell::new(None)),
        store,
    };

    gear.set_popover(Some(&build_config_popover(app, &widgets)));

    // ---- the wiring ---------------------------------------------------------
    // Each closure takes a full clone of `widgets`; the `connect_*` target
    // is itself a clone (same GObject), so no borrow of the local is held
    // across the move.
    {
        let widgets = widgets.clone();
        let app = app.clone();
        widgets.idle_start.clone().connect_clicked(move |_| {
            let config = widgets.store.borrow().config.clone();
            start_session(&widgets, &app, config);
        });
    }
    {
        let widgets = widgets.clone();
        let app = app.clone();
        widgets.pause_btn.clone().connect_clicked(move |_| {
            toggle_pause(&widgets, &app);
        });
    }
    {
        let widgets = widgets.clone();
        let app = app.clone();
        widgets.claim_btn.clone().connect_clicked(move |_| {
            claim_break(&widgets, &app);
        });
    }
    {
        let widgets = widgets.clone();
        let app = app.clone();
        widgets.skip_btn.clone().connect_clicked(move |_| {
            skip_phase(&widgets, &app);
        });
    }
    {
        let widgets = widgets.clone();
        let app = app.clone();
        widgets.stop_btn.clone().connect_clicked(move |_| {
            confirm_stop(&widgets, &app);
        });
    }
    {
        let widgets = widgets.clone();
        let app = app.clone();
        widgets.banner_action.clone().connect_clicked(move |_| {
            let action = {
                let store = widgets.store.borrow();
                store.banner.as_ref().and_then(|b| b.action)
            };
            if action == Some(BannerAction::ClaimBreak) {
                claim_break(&widgets, &app);
            } else {
                hide_banner(&widgets);
            }
        });
    }
    {
        let widgets = widgets.clone();
        widgets
            .banner_dismiss
            .clone()
            .connect_clicked(move |_| {
                hide_banner(&widgets);
            });
    }
    {
        // Tapping the banner's text retires it — the "tap anywhere" of the
        // iOS card, minus the button half of the row.
        let widgets = widgets.clone();
        let banner_text = widgets.banner_text.clone();
        let click = gtk4::GestureClick::new();
        click.connect_pressed(move |_, _, _, _| hide_banner(&widgets));
        banner_text.add_controller(click);
    }

    // Reconciliation news first, then the heartbeat: the banner raised on
    // reopen is the announcement for the phases that elapsed while away.
    {
        let widgets = widgets.clone();
        let app = app.clone();
        glib::idle_add_local_once(move || {
            let news = widgets.store.borrow_mut().take_reconcile_news();
            match news {
                Some(ReconcileNews::SessionDone) => {
                    show_banner(&widgets, session_complete_banner(&widgets, &app));
                }
                Some(ReconcileNews::BreakOver) => {
                    show_banner(&widgets, resume_banner(true));
                }
                None => {}
            }
            paint_card(&widgets, &app);
        });
    }
    {
        // The one-second heartbeat — cheap when nothing is due (a guard and
        // a compare), real work only on transitions. It runs for the app's
        // whole life so a phase ending while the reader is anywhere in the
        // window surfaces its banner from there.
        let widgets = widgets.clone();
        let app = app.clone();
        glib::timeout_add_local(std::time::Duration::from_secs(1), move || {
            tick(&widgets, &app);
            glib::ControlFlow::Continue
        });
    }

    paint_card(&widgets, app);
    widgets
}



#[cfg(test)]
mod tests {
    use super::*;
    // `TimeZone` already arrives through `super::*`; `Timelike` is the only
    // extra trait these tests need.
    use chrono::{TimeZone, Timelike};

    /// Fixed "now" for every test: an arbitrary afternoon instant.
    const T0: Unix = 1_760_000_000;

    fn classic_config() -> Config {
        Config {
            mode: Mode::Classic,
            prep_seconds: 0,
            classic_focus_minutes: vec![25, 25, 25, 25],
            classic_break_minutes: vec![5, 5, 5],
            ..Default::default()
        }
    }

    fn structured_config() -> Config {
        Config {
            mode: Mode::Structured,
            prep_seconds: 0,
            structured_by_duration: true,
            structured_duration_seconds: 3 * 3600,
            structured_break_count: 3,
            structured_break_seconds: 5 * 60,
            ..Default::default()
        }
    }

    fn flow_config() -> Config {
        Config {
            mode: Mode::Flowmodoro,
            prep_seconds: 0,
            ..Default::default()
        }
    }

    /// The classic timeline is prep-less here: focus, break, focus, break,
    /// focus, break, focus — the ends chain off the configured minutes.
    #[test]
    fn classic_full_run() {
        let mut s = Session::start(classic_config(), T0);
        let events = s.advance_if_due(T0 + (25 + 5 + 25 + 5 + 25 + 5 + 25) * 60);
        assert_eq!(s.phase, Phase::Done);
        assert_eq!(events.iter().filter(|e| e.completed_phase == Phase::Focus).count(), 4);
        assert_eq!(events.iter().filter(|e| e.completed_phase == Phase::Rest).count(), 3);
        // The focus events carry their own lengths.
        assert_eq!(events[0].seconds, 25.0 * 60.0);
    }

    /// A break follows each block while the break array has entries — a
    /// final block with no break left ends the session on focus.
    #[test]
    fn classic_fewer_breaks_than_blocks() {
        let config = Config {
            classic_focus_minutes: vec![25, 25, 25],
            classic_break_minutes: vec![5, 5],
            ..classic_config()
        };
        let mut s = Session::start(config.clone(), T0);
        s.advance_if_due(T0 + (25 + 5 + 25 + 5 + 25) * 60);
        assert_eq!(s.phase, Phase::Done);
        // The last event is the third focus, not a break.
        let mut s2 = Session::start(config, T0);
        let events = s2.advance_if_due(T0 + (25 + 5 + 25 + 5 + 25) * 60);
        assert_eq!(events.last().unwrap().completed_phase, Phase::Focus);
    }

    /// A session left alone for hours reconciles from the wall clock alone:
    /// every phase that elapsed while away is reported in order.
    #[test]
    fn classic_reconcile_after_long_gap() {
        let mut s = Session::start(classic_config(), T0);
        // The app "died" for 40 minutes — one call catches it up.
        let events = s.advance_if_due(T0 + 40 * 60);
        assert_eq!(events.len(), 2); // focus 1, break 1
        assert_eq!(s.phase, Phase::Focus);
        assert_eq!(s.step_index, 2);
    }

    /// Structured: the window is carved into even focus segments with the
    /// configured breaks between, and the last focus ends exactly at the
    /// window end.
    #[test]
    fn structured_duration_timeline() {
        let config = structured_config();
        let s = Session::start(config, T0);
        // 3 h window, 3 × 5 min breaks: (10800 - 900) / 4 = 2475 s focus.
        let mut ends = vec![T0];
        ends.extend(s.steps.iter().map(|st| st.end.unwrap()));
        let segments: Vec<i64> = ends
            .windows(2)
            .map(|w| w[1] - w[0])
            .collect();
        // prep-less: focus(2475) rest(300) focus(2475) rest(300) focus(2475)
        // rest(300) focus(2475)
        assert_eq!(s.steps.len(), 7);
        assert_eq!(segments, vec![2475, 300, 2475, 300, 2475, 300, 2475]);
        // The final focus ends exactly at the window end.
        assert_eq!(s.steps.last().unwrap().end, Some(T0 + 3 * 3600));
    }

    /// The finish-time window resolves to the *next* occurrence of the
    /// stored wall-clock time, today or tomorrow.
    #[test]
    fn structured_finish_time_window() {
        let config = Config {
            structured_by_duration: false,
            structured_finish_hour: 23,
            structured_finish_minute: 30,
            structured_break_count: 1,
            structured_break_seconds: 300,
            ..structured_config()
        };
        let s = Session::start(config, T0);
        let end = s.window_end.unwrap();
        assert!(end > T0);
        // The end sits at 23:30 local on the day of T0 (or the next, if the
        // test ran after 23:30) — either way, on the correct side of now.
        let local = chrono::Local.timestamp_opt(end, 0).single().unwrap();
        assert_eq!(local.hour() as i64, 23);
        assert_eq!(local.minute() as i64, 30);
    }

    /// A window too small for prep + breaks + a minute per focus segment
    /// builds no timeline, and the config refuses to start.
    #[test]
    fn structured_too_small_window() {
        let config = Config {
            structured_duration_seconds: 10 * 60, // less than prep+breaks
            structured_break_count: 3,
            structured_break_seconds: 5 * 60,
            prep_seconds: 180,
            ..structured_config()
        };
        assert_eq!(Session::start(config.clone(), T0).steps.len(), 0);
        assert!(!config.can_start(T0));
    }

    /// Skipping a structured phase ends only that phase: the window is
    /// absolute, later boundaries stay put.
    #[test]
    fn structured_skip_keeps_window() {
        let config = structured_config();
        let mut s = Session::start(config, T0);
        let rest_end_before = s.steps[1].end.unwrap();
        s.advance_if_due(T0 + 100); // into focus 1
        let window_end = s.window_end.unwrap();
        assert!(s.skip_phase(T0 + 200));
        assert_eq!(s.steps[0].end, Some(T0 + 200)); // the skipped focus ends now
        assert_eq!(s.steps[1].end, Some(rest_end_before)); // the break keeps its end
        assert_eq!(s.window_end, Some(window_end));
    }

    /// Pausing freezes every boundary; resuming slides each pending one by
    /// the paused span.
    #[test]
    fn pause_slides_current_and_future_boundaries() {
        let mut s = Session::start(classic_config(), T0);
        s.advance_if_due(T0 + 60);
        let end_before = s.phase_end.unwrap();
        assert!(s.pause(T0 + 120));
        // Nothing advances while paused, however late the caller asks.
        assert!(s.advance_if_due(T0 + 3 * 3600).is_empty());
        assert!(s.resume(T0 + 600)); // paused for 480 s
        assert_eq!(s.phase_end, Some(end_before + 480));
    }

    /// The flow pot: accrual, the claim threshold, and the reset-on-claim.
    #[test]
    fn flowmodoro_accrual_and_claim() {
        let mut s = Session::start(flow_config(), T0);
        // Under the threshold: no claim.
        assert!(!s.flow_can_claim(T0 + 100));
        assert!(!s.claim_break(T0 + 100));
        // At 3 minutes: claimable. 3 min focus = 36 s of break.
        assert!(s.flow_can_claim(T0 + 180));
        let earned = s.flow_earned_break(T0 + 180);
        assert!((earned - 36.0).abs() < 0.001);
        assert!(s.claim_break(T0 + 180));
        assert_eq!(s.phase, Phase::Rest);
        assert_eq!(s.phase_end, Some(T0 + 180 + 36));
        // The pot reset: after the break, focus accrues from zero.
        s.advance_if_due(T0 + 180 + 36);
        assert_eq!(s.phase, Phase::Focus);
        assert_eq!(s.flow_accrued_focus(T0 + 180 + 36 + 10), 10.0);
    }

    /// Pausing flow focus banks the live stint first, so accrual stops
    /// exactly at the pause.
    #[test]
    fn flowmodoro_pause_stops_accrual() {
        let mut s = Session::start(flow_config(), T0);
        s.pause(T0 + 60);
        // Time passes while paused; the pot does not grow.
        assert_eq!(s.flow_accrued_focus(T0 + 3600), 60.0);
        s.resume(T0 + 3660);
        assert_eq!(s.flow_accrued_focus(T0 + 3670), 70.0);
    }

    /// Flow focus cannot be skipped — it has no "next" without a claim —
    /// but a rest can.
    #[test]
    fn flowmodoro_skip_rest() {
        let mut s = Session::start(flow_config(), T0);
        assert!(!s.skip_phase(T0 + 10)); // focus: nothing to skip
        assert!(s.claim_break(T0 + 180));
        assert!(s.skip_phase(T0 + 190));
        assert_eq!(s.phase, Phase::Focus);
        assert_eq!(s.flow_accrued_focus(T0 + 191), 1.0);
    }

    /// Classic skip slides the whole schedule forward by the reclaimed
    /// time; structured's skip (tested above) does not.
    #[test]
    fn classic_skip_slides_schedule() {
        let mut s = Session::start(classic_config(), T0);
        let break_end_before = s.steps[1].end.unwrap();
        assert!(s.skip_phase(T0 + 5 * 60)); // skip 20 min of focus 1
        assert_eq!(s.steps[0].end, Some(T0 + 5 * 60));
        assert_eq!(s.steps[1].end, Some(break_end_before - 20 * 60));
    }

    /// Stopping mid-focus reports the partial duration, and it folds into
    /// the day log.
    #[test]
    fn stop_mid_focus_reports_partial() {
        let mut s = Session::start(classic_config(), T0);
        s.advance_if_due(T0 + 60);
        let event = s.stop(T0 + 600).unwrap();
        assert_eq!(event.completed_phase, Phase::Focus);
        // 10 min into the 25-min block — the partial, not the block length.
        // (A prep-less session's focus began at T0.)
        assert_eq!(event.seconds, 600.0);
        assert_eq!(s.phase, Phase::Done);
    }

    /// Stopping during prep reports the prep seconds.
    #[test]
    fn stop_mid_prep() {
        let config = Config { prep_seconds: 180, ..classic_config() };
        let mut s = Session::start(config, T0);
        let event = s.stop(T0 + 30).unwrap();
        assert_eq!(event.completed_phase, Phase::Prep);
        assert_eq!(event.seconds, 30.0);
    }

    /// Stopping an open-ended flow focus counts the whole accrued pot.
    #[test]
    fn stop_flow_focus_counts_accrued() {
        let mut s = Session::start(flow_config(), T0);
        let event = s.stop(T0 + 750).unwrap();
        assert_eq!(event.completed_phase, Phase::Focus);
        assert_eq!(event.seconds, 750.0);
    }

    /// The phase fill is bounded 0..1 for fixed phases and cycles once per
    /// earned break minute for the open flow focus.
    #[test]
    fn phase_progress_bounds() {
        let s = Session::start(classic_config(), T0);
        assert_eq!(s.phase_progress(T0), 0.0);
        assert_eq!(s.phase_progress(T0 + 25 * 60), 1.0);
        // Flow focus: the fill laps every 300 s.
        let f = Session::start(flow_config(), T0);
        assert_eq!(f.phase_progress(T0 + 150), 0.5);
        assert!((f.phase_progress(T0 + 450) - 0.5).abs() < 1e-9);
    }

    /// The state file round-trips, and an older file without the flow
    /// fields restores instead of wiping.
    #[test]
    fn state_file_round_trip_and_lenient_decode() {
        let mut s = Session::start(classic_config(), T0);
        s.advance_if_due(T0 + 60);
        let file = StateFile {
            config: Some(classic_config()),
            session: Some(s.clone()),
            history: vec![DayRecord {
                day_key: "2026-10-25".into(),
                focus_seconds: 60.0,
                blocks: 1,
            }],
        };
        let json = serde_json::to_string(&file).unwrap();
        let back: StateFile = serde_json::from_str(&json).unwrap();
        assert_eq!(back.session.unwrap(), s);
        assert_eq!(back.history.len(), 1);

        // An older writer that never heard of flowmodoro: the session
        // restores with the defaults filled in. The session's snapshot
        // config is not optional, so the old writer carries a real one —
        // flow-less fields decode through `serde(default)`.
        let old = format!(
            r#"{{"config":{{"mode":"classic","prepSeconds":0,"structuredByDuration":true,"structuredDurationSeconds":10800,"structuredFinishHour":17,"structuredFinishMinute":0,"structuredBreakCount":3,"structuredBreakSeconds":300,"classicFocusMinutes":[25],"classicBreakMinutes":[5],"chime":true}},"session":{{"config":{{"mode":"classic","prepSeconds":0}},"startedAt":{T0},"phase":"focus","phaseStart":{T0},"phaseEnd":{},"steps":[],"stepIndex":0}}}}"#,
            T0 + 1500
        );
        let lenient: StateFile = serde_json::from_str(&old).unwrap();
        let restored = lenient.session.unwrap();
        assert_eq!(restored.phase, Phase::Focus);
        assert_eq!(restored.flow_banked, 0.0);
    }

    /// The countdown text rounds up: a timer must never show 0:00 early.
    #[test]
    fn countdown_rounds_up() {
        assert_eq!(countdown_text(59.2), "01:00");
        assert_eq!(countdown_text(0.0), "00:00");
        assert_eq!(countdown_text(3661.0), "1:01:01");
    }

    /// The structured summary is the start gate: a window that cannot fit
    /// the plan has none.
    #[test]
    fn structured_summary_gates_start() {
        let good = structured_config();
        assert!(good.can_start(T0));
        assert!(good.structured_summary(T0).is_some());
        let bad = Config {
            structured_duration_seconds: 10 * 60,
            structured_break_count: 3,
            structured_break_seconds: 5 * 60,
            prep_seconds: 180,
            ..structured_config()
        };
        assert!(bad.structured_summary(T0).is_none());
        assert!(!bad.can_start(T0));
    }
}
