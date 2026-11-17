//! The mini-player: TTS playback, read-along and bookmarks on the UI thread.
//!
//! The player owns no audio. It starts a [`sn_tts::SessionCore`] (whose
//! threads do the synthesis and the sink writes) and then polls its progress
//! on a 100 ms glib tick. Each tick does three things, in this order of
//! trustworthiness:
//!
//! 1. **Bookmark** — written *from* the session's frontier, never the other
//!    way round. That is the write-ahead-bookmark rule: the frontier is the
//!    pipeline's field, the store only receives it.
//! 2. **Read-along** — the plan chunk owning the frontier is highlighted in
//!    the editor. The plan's byte offsets are the note's byte offsets (the
//!    sanitiser is length-preserving), so the only translation is bytes →
//!    GTK chars, done once per sentence change, not per tick.
//! 3. **Widgets** — labels, counts, the play/pause icon.
//!
//! Pausing is the session's business (it stops its own sink at the heard
//! edge); resuming rebuilds its plan, and the player notices the new
//! `plan_epoch` and re-fetches chunk boundaries. A speed change cannot reach
//! a chunk already in an engine, so it stops the session and queues a
//! restart at the current frontier — served on the tick, so no session is
//! ever replaced from inside a signal handler.

use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::Label;

use crate::app::App;


mod autoplay;
mod bookmark;
mod highlight;
mod session;
mod speak_from;
mod transport;

// Re-exported wholesale so every existing `crate::player::*` path keeps
// resolving unchanged. Glob re-exports preserve each item's own visibility.
pub use autoplay::*;
pub use session::*;
pub use speak_from::*;
pub use transport::*;

/// Poll interval. Fast enough that read-along feels attached to the voice,
/// slow enough that a tick is nothing.
const TICK_MS: u64 = 100;

/// Highlight paint: the accent blue at a low alpha, behind the spoken text.
const HIGHLIGHT: (f32, f32, f32, f32) = (0.29, 0.51, 0.87, 0.25);

pub const TAG_READ_ALONG: &str = "read-along";

/// The player's widgets, built once with the window and reachable like any
/// other pane's.
#[derive(Clone)]
pub struct PlayerWidgets {
    pub bar: gtk4::Box,
    pub restart: gtk4::Button,
    /// The play/pause button, whose glyph crossfades between the two
    /// shapes rather than snapping from one to the other.
    pub play: crate::motion::Glyph,
    pub stop: gtk4::Button,
    /// Whole-session progress: sentences through a note, chapters through an
    /// audiobook. Between Stop and the status so the eye meets it mid-row.
    /// A slider, not a display-only fill: when a book owns the bar the
    /// handle is there to be grabbed.
    pub progress: crate::abook::ScrubSlider,
    pub status: Label,
    pub position: Label,
    pub speed: gtk4::SpinButton,
}

/// Build the mini-player bar and wire its buttons. Hidden until playback
/// starts; it stays visible after a stop so the final state can be read.
pub fn build(app: &Rc<App>) -> PlayerWidgets {
    let restart = gtk4::Button::from_icon_name("media-skip-backward-symbolic");
    restart.set_tooltip_text(Some("Restart from the beginning"));
    let play = crate::motion::Glyph::new();
    play.button.set_tooltip_text(Some("Speak (F5)"));
    let stop = gtk4::Button::from_icon_name("media-playback-stop-symbolic");
    stop.set_tooltip_text(Some("Stop (F6)"));

    // The progress slider is the row's elastic element: it takes whatever
    // room is left and gives it back when there is not enough.
    //
    // **Why.** The transport bar's contents add up to 683 px — three
    // buttons (38 each), the speed dial (116) and the engine/voice
    // pickers (223) — and that total was the app's *minimum window
    // width*: the window refused to go below 693 px, and on a narrower
    // screen the whole layout overflowed to the right instead of
    // compressing. A slider that can shrink is the right place to
    // give that room back: for a note it is a display, and a 90 px
    // slider still reads.
    //
    // It is a scale, not a display-only fill: an audiobook owns the bar
    // whenever one is live (or shown on the reader's page), and then the
    // handle is there to be grabbed — a drag aims through the whole book and
    // commits one seek when it rests. While a note is speaking the hand
    // snaps back to the position the poll wrote; the slider stays a display.
    let progress = crate::abook::ScrubSlider::new(app, true);
    progress.add_css_class("transport-progress");
    progress.widget().set_width_request(90);

    let status = Label::builder()
        .halign(gtk4::Align::Start)
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        .build();
    status.set_hexpand(true);
    // A monospace-ish position readout via CSS class; the theme styles it.
    let position = Label::builder()
        .halign(gtk4::Align::End)
        .css_classes(["numeric"])
        .build();

    let speed = gtk4::SpinButton::with_range(0.5, 2.0, 0.05);
    speed.set_digits(2);
    speed.set_value(app.prefs.get().rate_multiplier);
    // Seed the "what the spin shows" memory with the same value, so the first
    // turn of the dial is not mistaken for a programmatic set.
    app.shown_speed.set(app.prefs.get().rate_multiplier);
    speed.set_tooltip_text(Some("Speaking speed"));
    // The dial shows "1.00" and its up/down arrows; that needs far less
    // than the 116 px it asked for, and the transport bar's width is the
    // app's minimum window width. A capped width keeps the digits and the
    // arrows and gives the rest of the row its room back.
    speed.set_width_request(72);

    let bar = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(6)
        .css_classes(["toolbar"])
        .name("mini-player")
        .build();
    bar.append(&restart);
    bar.append(&play.button);
    bar.append(&stop);
    bar.append(progress.widget());
    bar.append(&status);
    bar.append(&position);
    bar.append(&speed);
    // Always visible: this bar is the app's transport — the play button is
    // how TTS starts, and the engine/voice pickers live here. Hiding it
    // until the first play made the controls undiscoverable.
    bar.set_visible(true);

    {
        let app = app.clone();
        play.button.connect_clicked(move |_| {
            // The transport drives whichever session is live: an audiobook
            // when one is open — playing or stopped on the reader's page,
            // `maybe_toggle` resumes the stopped one — the note player
            // otherwise. The bar is the app's transport, so leaving it wired
            // only to the note player made the bottom controls dead on a
            // book page.
            if !crate::abook::maybe_toggle(&app) {
                toggle_play(&app);
            }
        });
    }
    {
        let app = app.clone();
        stop.connect_clicked(move |_| {
            if crate::abook::is_live(&app) {
                crate::abook::close(&app);
            } else {
                stop_playback(&app);
            }
        });
    }
    {
        let app = app.clone();
        restart.connect_clicked(move |_| {
            if crate::abook::is_live(&app) {
                // The tooltip promises the beginning, and a listener pressing
                // it on a 3-hour chapter means it: position-preserving
                // re-opens (speed changes) go through the same `open`, with
                // the offset, not through here.
                crate::abook::restart_from_beginning(&app);
            } else {
                restart_from_beginning(&app);
            }
        });
    }
    {
        let app = app.clone();
        speed.connect_value_changed(move |spin| {
            let value = spin.value();
            // A programmatic set_value — showing the live session's rate when
            // a book opens — must not read as the user turning the dial. Both
            // restarts are expensive (a new ffmpeg child) and one fired from
            // here would kill the session that had just started.
            if (value - shown_rate(&app)).abs() < 0.001 {
                return;
            }
            app.prefs.update(|p| p.rate_multiplier = value);
            // An audiobook's rate lives in its own pref, and the pipe's is
            // fixed at spawn — so the change takes effect on the next open,
            // and a running session restarts from where the listener is.
            if crate::abook::is_live(&app) {
                app.prefs.update(|p| p.audio_rate = value);
                crate::abook::restart(&app);
                return;
            }
            // A chunk already in the engine speaks at its old rate; the
            // restart applies the new one from where the voice is now. Paused
            // counts too: resume() reuses the session's engine, so a paused
            // session would otherwise come back at the old speed.
            if let Some(session) = app.tts.borrow().as_ref() {
                let progress = session.progress();
                if matches!(
                    progress.state,
                    sn_tts::PlayState::Playing | sn_tts::PlayState::Paused
                ) {
                    app.tts_restart.set(Some(progress.frontier));
                    session.stop();
                }
            }
        });
    }

    PlayerWidgets {
        bar,
        restart,
        play,
        stop,
        progress,
        status,
        position,
        speed,
    }
}
